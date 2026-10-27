//! Lyrics search across LRCLIB and KuGou, with candidates ranked by how well their title,
//! artist and duration match the song.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde::Deserialize;

use super::lrc::{format_timestamp, parse_lrc, plain_from_text, to_lrc};
use super::sources::{derive_title, new_agent, primary_artist, LRCLIB_BASE};
use super::{Lyrics, Provider, SongKey};

/// KuGou only answers over plain HTTP.
const KUGOU_BASE: &str = "http://lyrics.kugou.com";
/// Candidates downloaded from KuGou per manual search; each needs its own request.
pub const KUGOU_SEARCH_LIMIT: usize = 4;
/// Candidates downloaded from KuGou by the automatic lookup.
pub(crate) const KUGOU_AUTO_LIMIT: usize = 2;
/// Maximum duration mismatch for synced lyrics to be trusted without checking.
pub const DURATION_TOLERANCE: Duration = Duration::from_secs(5);
/// Match score below which a candidate is likely another song.
pub const ACCEPT_SCORE: f32 = 0.7;
/// Match score at which title and artist both match; a same-titled song by another artist
/// stays below it.
pub const SURE_SCORE: f32 = 0.9;
/// Score multiplier for instrumental or karaoke versions when the song itself is not one.
const INSTRUMENTAL_PENALTY: f32 = 0.8;
/// Title words dropped from the song's title unless they make up all of it.
const TITLE_NOISE: [&str; 11] = [
    "official",
    "video",
    "audio",
    "lyrics",
    "lyric",
    "music",
    "hd",
    "hq",
    "4k",
    "visualizer",
    "mv",
];

/// What to search for. Built from a song with [`SearchRequest::from_key`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchRequest {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The song's duration; ranks candidates and enables duration checks.
    pub duration: Option<Duration>,
    /// Free-text query that replaces the title and artist queries; candidates are then scored
    /// against its words.
    pub query: Option<String>,
}

impl SearchRequest {
    pub fn from_key(key: &SongKey) -> Self {
        Self {
            title: derive_title(key),
            artist: key.artist.trim().to_owned(),
            album: key.album.trim().to_owned(),
            duration: key.duration,
            query: None,
        }
    }

    /// The custom query, when set and not blank.
    fn custom_query(&self) -> Option<&str> {
        self.query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
    }

    fn scorer(&self) -> Scorer {
        match self.custom_query() {
            Some(q) => Scorer::new(q, ""),
            None => Scorer::new(&self.title, &self.artist),
        }
    }

    /// Artist for provider queries: the primary artist without bracketed notes.
    fn query_artist(&self) -> String {
        strip_brackets(&primary_artist(&self.artist))
            .trim()
            .to_owned()
    }
}

/// One set of lyrics found by a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub provider: Provider,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<Duration>,
    /// LRC text for synced lyrics, with `[ti:]`, `[ar:]`, `[al:]` and `[length:]` tags when
    /// known; plain text otherwise. This is the text to save as a manual override.
    pub text: String,
    pub lyrics: Arc<Lyrics>,
    /// Title and artist similarity to the request, from 0 to 1.
    pub score: f32,
    /// Distance between the candidate's and the song's duration, when both are known.
    pub duration_diff: Option<Duration>,
}

impl Candidate {
    pub fn synced(&self) -> bool {
        matches!(*self.lyrics, Lyrics::Synced(_))
    }

    /// Title and artist match closely enough to use without asking.
    pub fn is_sure_match(&self) -> bool {
        self.score >= SURE_SCORE
    }

    /// Likely the same song, possibly by differently credited artists.
    pub fn is_good_match(&self) -> bool {
        self.score >= ACCEPT_SCORE
    }

    /// The duration is unknown on either side, or within [`DURATION_TOLERANCE`].
    pub fn duration_matches(&self) -> bool {
        self.duration_diff.is_none_or(|d| d <= DURATION_TOLERANCE)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderError {
    pub provider: Provider,
    pub message: String,
}

/// Result of a search. No candidates and no failures means nothing was found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchOutcome {
    /// Best first: by match score, then synced before plain, then closest duration.
    /// Candidates with identical lyrics are listed once.
    pub candidates: Vec<Candidate>,
    /// Providers whose requests failed; the others' candidates are still listed.
    pub failures: Vec<ProviderError>,
    /// LRCLIB's best sure match is marked instrumental.
    pub instrumental: bool,
}

/// Searches LRCLIB and KuGou in parallel. Blocks until both finish.
pub fn search(agent: &ureq::Agent, req: &SearchRequest) -> SearchOutcome {
    search_with(agent, req, &|| false)
}

/// [`search`] that stops issuing requests once `cancelled` returns true.
pub fn search_with(
    agent: &ureq::Agent,
    req: &SearchRequest,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> SearchOutcome {
    let (lrclib, kugou) = search_providers(agent, req, KUGOU_SEARCH_LIMIT, cancelled);
    let mut outcome = SearchOutcome::default();
    match lrclib {
        Ok(found) => {
            outcome.candidates.extend(found.candidates);
            outcome.instrumental = found.instrumental;
        }
        Err(e) => outcome.failures.push(ProviderError {
            provider: Provider::Lrclib,
            message: e.to_string(),
        }),
    }
    match kugou {
        Ok(found) => outcome.candidates.extend(found),
        Err(e) => outcome.failures.push(ProviderError {
            provider: Provider::Kugou,
            message: e.to_string(),
        }),
    }
    rank(&mut outcome.candidates);
    dedup(&mut outcome.candidates);
    outcome
}

pub(crate) struct LrclibFound {
    /// Ranked best first.
    pub candidates: Vec<Candidate>,
    pub instrumental: bool,
}

/// Runs the LRCLIB and KuGou searches on two threads.
pub(crate) fn search_providers(
    agent: &ureq::Agent,
    req: &SearchRequest,
    kugou_limit: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> (
    Result<LrclibFound, ureq::Error>,
    Result<Vec<Candidate>, ureq::Error>,
) {
    let scorer = req.scorer();
    thread::scope(|s| {
        let kugou = s.spawn(|| search_kugou(agent, req, &scorer, kugou_limit, cancelled));
        let lrclib = search_lrclib(agent, req, &scorer, cancelled);
        let kugou = kugou.join().unwrap_or_else(|_| Ok(Vec::new()));
        (lrclib, kugou)
    })
}

/// First synced, sure match within the duration tolerance of a ranked list.
pub(crate) fn best_synced(candidates: &[Candidate]) -> Option<&Candidate> {
    candidates
        .iter()
        .find(|c| c.synced() && c.is_sure_match() && c.duration_matches())
}

/// Sorts best first: match score, synced before plain, then closest duration.
fn rank(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.synced().cmp(&a.synced()))
            .then_with(|| match (a.duration_diff, b.duration_diff) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
}

/// Drops candidates whose lyrics equal an earlier candidate's.
fn dedup(candidates: &mut Vec<Candidate>) {
    let mut seen = HashSet::new();
    candidates.retain(|c| seen.insert(to_lrc(&c.lyrics)));
}

fn duration_diff(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    let (a, b) = (a?, b?);
    Some(a.abs_diff(b))
}

// ---------------------------------------------------------------------------------------------
// LRCLIB

/// A track record from the LRCLIB API.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LrclibTrack {
    #[serde(default)]
    pub track_name: Option<String>,
    #[serde(default)]
    pub artist_name: Option<String>,
    #[serde(default)]
    pub album_name: Option<String>,
    #[serde(default)]
    pub synced_lyrics: Option<String>,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
    /// Seconds.
    #[serde(default)]
    pub duration: Option<f64>,
}

impl LrclibTrack {
    fn duration(&self) -> Option<Duration> {
        self.duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| Duration::from_millis((d * 1000.0).round() as u64))
    }

    fn text(field: &Option<String>) -> Option<&str> {
        field.as_deref().filter(|s| !s.trim().is_empty())
    }

    pub fn synced(&self) -> Option<Lyrics> {
        Self::text(&self.synced_lyrics)
            .filter(|_| !self.instrumental)
            .map(parse_lrc)
            .filter(|l| matches!(l, Lyrics::Synced(_)))
    }

    pub fn plain(&self) -> Option<Lyrics> {
        Self::text(&self.plain_lyrics)
            .filter(|_| !self.instrumental)
            .map(plain_from_text)
            .filter(|l| !l.is_empty())
    }

    /// Synced lyrics if present, else plain. None for instrumentals or empty records.
    pub fn lyrics(&self) -> Option<Lyrics> {
        self.synced().or_else(|| self.plain())
    }

    /// The record as a candidate; None when it holds no lyrics.
    fn candidate(&self, scorer: &Scorer, target: Option<Duration>) -> Option<Candidate> {
        let lyrics = self.lyrics()?;
        let title = self.track_name.clone().unwrap_or_default();
        let artist = self.artist_name.clone().unwrap_or_default();
        let duration = self.duration();
        Some(Candidate {
            provider: Provider::Lrclib,
            score: scorer.score(&title, &artist),
            text: lrclib_text(self),
            title,
            artist,
            album: self.album_name.clone().unwrap_or_default(),
            duration,
            lyrics: Arc::new(lyrics),
            duration_diff: duration_diff(duration, target),
        })
    }
}

/// Text of a record's lyrics: LRC with `[ar:]`, `[al:]`, `[ti:]` and `[length:]` tags for
/// synced lyrics, else the plain text.
pub fn lrclib_text(track: &LrclibTrack) -> String {
    if track.synced().is_some() {
        let body = LrclibTrack::text(&track.synced_lyrics).unwrap_or_default();
        lrc_with_header(
            body,
            track.track_name.as_deref().unwrap_or_default(),
            track.artist_name.as_deref().unwrap_or_default(),
            track.album_name.as_deref().unwrap_or_default(),
            track.duration(),
        )
    } else {
        LrclibTrack::text(&track.plain_lyrics)
            .unwrap_or_default()
            .trim()
            .to_owned()
    }
}

/// `[ti:]`, `[ar:]`, `[al:]` and `[length:]` tags (each when known and absent from `body`)
/// followed by `body`.
fn lrc_with_header(
    body: &str,
    title: &str,
    artist: &str,
    album: &str,
    length: Option<Duration>,
) -> String {
    let has = |tag: &str| super::lrc::tag_value(body, tag).is_some();
    let mut out = String::new();
    for (tag, value) in [("ar", artist), ("al", album), ("ti", title)] {
        if !value.trim().is_empty() && !has(tag) {
            out.push_str(&format!("[{tag}:{}]\n", value.trim()));
        }
    }
    if let Some(len) = length.filter(|_| !has("length")) {
        out.push_str(&format!("[length:{}]\n", format_timestamp(len)));
    }
    out.push_str(body.trim());
    out
}

fn lrclib_search(
    agent: &ureq::Agent,
    params: &[(&str, &str)],
) -> Result<Vec<LrclibTrack>, ureq::Error> {
    let mut req = agent.get(format!("{LRCLIB_BASE}/search"));
    for (k, v) in params {
        req = req.query(*k, *v);
    }
    match req.call() {
        Ok(mut resp) => resp.body_mut().read_json::<Vec<LrclibTrack>>(),
        Err(ureq::Error::StatusCode(404)) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Candidates from LRCLIB's search and whether the best sure match is an instrumental. A
/// custom query is sent as `q`; otherwise the title and artist are sent as fields, with a
/// `q` search as the fallback when that finds no good match.
fn search_lrclib(
    agent: &ureq::Agent,
    req: &SearchRequest,
    scorer: &Scorer,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<LrclibFound, ureq::Error> {
    let mut tracks = Vec::new();
    if let Some(q) = req.custom_query() {
        tracks = lrclib_search(agent, &[("q", q)])?;
    } else if !req.title.is_empty() {
        let artist = req.query_artist();
        let mut params = vec![("track_name", req.title.as_str())];
        if !artist.is_empty() {
            params.push(("artist_name", artist.as_str()));
        }
        tracks = lrclib_search(agent, &params)?;
        let good = |t: &LrclibTrack| {
            let title = t.track_name.as_deref().unwrap_or_default();
            let artist = t.artist_name.as_deref().unwrap_or_default();
            scorer.score(title, artist) >= ACCEPT_SCORE
        };
        if !tracks.iter().any(good) && !cancelled() {
            let q = format!("{artist} {}", req.title);
            tracks.extend(lrclib_search(agent, &[("q", q.trim())])?);
        }
    }
    Ok(lrclib_found(&tracks, scorer, req.duration))
}

fn lrclib_found(tracks: &[LrclibTrack], scorer: &Scorer, target: Option<Duration>) -> LrclibFound {
    // The first best sure match, skipping separately released instrumental versions.
    let mut best: Option<(f32, &LrclibTrack)> = None;
    for t in tracks {
        let title = t.track_name.as_deref().unwrap_or_default();
        if is_instrumental_title(title) {
            continue;
        }
        let score = scorer.score(title, t.artist_name.as_deref().unwrap_or_default());
        if score >= SURE_SCORE && best.is_none_or(|(b, _)| score > b) {
            best = Some((score, t));
        }
    }
    let instrumental = best.is_some_and(|(_, t)| t.instrumental);
    let mut candidates: Vec<Candidate> = tracks
        .iter()
        .filter_map(|t| t.candidate(scorer, target))
        .collect();
    rank(&mut candidates);
    dedup(&mut candidates);
    LrclibFound {
        candidates,
        instrumental,
    }
}

// ---------------------------------------------------------------------------------------------
// KuGou

#[derive(Debug, Default, Deserialize)]
struct KugouSearch {
    #[serde(default)]
    candidates: Vec<KugouCandidate>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct KugouCandidate {
    #[serde(default)]
    id: serde_json::Value,
    #[serde(default)]
    accesskey: String,
    #[serde(default)]
    singer: String,
    #[serde(default)]
    song: String,
    /// Milliseconds.
    #[serde(default)]
    duration: Option<u64>,
}

impl KugouCandidate {
    fn id(&self) -> Option<String> {
        match &self.id {
            serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration.filter(|d| *d > 0).map(Duration::from_millis)
    }
}

#[derive(Debug, Default, Deserialize)]
struct KugouDownload {
    #[serde(default)]
    content: String,
}

/// LRC text from a download response's base64 `content`.
fn kugou_lrc(download: &KugouDownload) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(download.content.trim())
        .ok()?;
    let text = super::lrc::decode_text(&bytes);
    let text = text.trim();
    (!text.is_empty()).then(|| text.replace("\r\n", "\n"))
}

/// Candidates ranked best first and reduced to one per title, artist and duration, since
/// KuGou lists the same lyrics under several ids.
fn rank_kugou(
    mut found: Vec<KugouCandidate>,
    scorer: &Scorer,
    target: Option<Duration>,
) -> Vec<(f32, KugouCandidate)> {
    let mut seen = HashSet::new();
    found.retain(|c| {
        c.id().is_some()
            && !c.accesskey.is_empty()
            && seen.insert((c.song.clone(), c.singer.clone(), c.duration))
    });
    let mut scored: Vec<(f32, KugouCandidate)> = found
        .into_iter()
        .map(|c| (scorer.score(&c.song, &c.singer), c))
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        let diff =
            |c: &KugouCandidate| duration_diff(c.duration(), target).unwrap_or(Duration::MAX);
        sb.total_cmp(sa).then(diff(a).cmp(&diff(b)))
    });
    scored
}

fn search_kugou(
    agent: &ureq::Agent,
    req: &SearchRequest,
    scorer: &Scorer,
    limit: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<Candidate>, ureq::Error> {
    let keyword = match req.custom_query() {
        Some(q) => q.to_owned(),
        None => {
            let artist = req.query_artist();
            if artist.is_empty() {
                req.title.clone()
            } else {
                format!("{artist} - {}", req.title)
            }
        }
    };
    if keyword.trim().is_empty() || cancelled() {
        return Ok(Vec::new());
    }
    let duration_ms = req.duration.map_or(0, |d| d.as_millis()).to_string();
    let found = agent
        .get(format!("{KUGOU_BASE}/search"))
        .query("ver", "1")
        .query("man", "yes")
        .query("client", "pc")
        .query("keyword", &keyword)
        .query("duration", &duration_ms)
        .query("hash", "")
        .call()?
        .body_mut()
        .read_json::<KugouSearch>()?
        .candidates;
    let ranked = rank_kugou(found, scorer, req.duration);

    // Downloads run one after another over the search's kept-alive connection, which is
    // much faster than opening one per download. The first error is kept in case none
    // succeeds.
    let mut candidates = Vec::new();
    let mut error = None;
    for (score, c) in ranked.iter().take(limit) {
        match kugou_download(agent, c, *score, req.duration, cancelled) {
            Ok(Some(c)) => candidates.push(c),
            Ok(None) => {}
            Err(e) => error = error.or(Some(e)),
        }
    }
    match error {
        Some(e) if candidates.is_empty() => Err(e),
        _ => Ok(candidates),
    }
}

fn kugou_download(
    agent: &ureq::Agent,
    c: &KugouCandidate,
    score: f32,
    target: Option<Duration>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Option<Candidate>, ureq::Error> {
    if cancelled() {
        return Ok(None);
    }
    let download = agent
        .get(format!("{KUGOU_BASE}/download"))
        .query("ver", "1")
        .query("client", "pc")
        .query("id", c.id().unwrap_or_default())
        .query("accesskey", &c.accesskey)
        .query("fmt", "lrc")
        .query("charset", "utf8")
        .call()?
        .body_mut()
        .read_json::<KugouDownload>()?;
    Ok(kugou_candidate(c, &download, score, target))
}

fn kugou_candidate(
    c: &KugouCandidate,
    download: &KugouDownload,
    score: f32,
    target: Option<Duration>,
) -> Option<Candidate> {
    let body = kugou_lrc(download)?;
    let lyrics = parse_lrc(&body);
    if !matches!(lyrics, Lyrics::Synced(_)) {
        return None;
    }
    let duration = c.duration();
    let album = super::lrc::tag_value(&body, "al")
        .unwrap_or_default()
        .to_owned();
    Some(Candidate {
        provider: Provider::Kugou,
        text: lrc_with_header(&body, &c.song, &c.singer, "", duration),
        title: c.song.clone(),
        artist: c.singer.clone(),
        album,
        duration,
        lyrics: Arc::new(lyrics),
        score,
        duration_diff: duration_diff(duration, target),
    })
}

// ---------------------------------------------------------------------------------------------
// Matching

/// Lowercase text with featured-artist credits removed: bracketed groups that start with
/// `feat`, `ft`, `featuring` or `with`, and everything after ` feat. `, ` ft. ` or
/// ` featuring `.
fn strip_features(text: &str) -> String {
    const WORDS: [&str; 5] = ["feat.", "feat ", "ft.", "ft ", "featuring "];
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut rest = lower.as_str();
    while let Some(start) = rest.find(['(', '[']) {
        let close = if rest[start..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let inner_start = start + 1;
        let Some(len) = rest[inner_start..].find(close) else {
            break;
        };
        let inner = rest[inner_start..inner_start + len].trim_start();
        let is_credit = WORDS.iter().any(|w| inner.starts_with(w)) || inner.starts_with("with ");
        out.push_str(&rest[..start]);
        if !is_credit {
            out.push_str(&rest[start..inner_start + len + 1]);
        }
        rest = &rest[inner_start + len + 1..];
    }
    out.push_str(rest);
    let cut = WORDS
        .iter()
        .filter_map(|w| out.find(&format!(" {w}")))
        .min()
        .unwrap_or(out.len());
    out.truncate(cut);
    out
}

/// Text with every `(...)`, `[...]`, `{...}` and `【...】` group removed.
fn strip_brackets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' | '[' | '{' | '【' => depth += 1,
            ')' | ']' | '}' | '】' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Lowercase alphanumeric words; apostrophes are dropped so "don't" matches "dont".
fn words(text: &str) -> Vec<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`'))
        .flat_map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![' ']
            }
        })
        .collect();
    cleaned.split_whitespace().map(str::to_owned).collect()
}

/// Titles of separately released versions without vocals: `Song (Instrumental)`,
/// `Song [Off Vocal]`, `Song - Instrumental Ver.`, `Song (Karaoke)`.
pub fn is_instrumental_title(title: &str) -> bool {
    const MARKS: [&str; 4] = ["instrumental", "inst", "karaoke", "offvocal"];
    const SUFFIX_WORDS: [&str; 8] = [
        "instrumental",
        "inst",
        "karaoke",
        "offvocal",
        "off",
        "vocal",
        "ver",
        "version",
    ];
    let lower = title.to_lowercase();
    let marked = |w: &[String]| {
        w.iter().any(|t| MARKS.contains(&t.as_str()))
            || w.windows(2).any(|p| p[0] == "off" && p[1] == "vocal")
    };
    // Bracketed notes.
    let mut depth = 0usize;
    let mut inner = String::new();
    for c in lower.chars() {
        match c {
            '(' | '[' | '{' | '【' | '「' => {
                depth += 1;
                inner.push(' ');
            }
            ')' | ']' | '}' | '】' | '」' => depth = depth.saturating_sub(1),
            _ if depth > 0 => inner.push(c),
            _ => {}
        }
    }
    if marked(&words(&inner)) {
        return true;
    }
    // A dash-separated suffix made only of version words.
    lower
        .char_indices()
        .filter(|&(i, c)| i > 0 && matches!(c, '-' | '–' | '—' | '~'))
        .any(|(i, _)| {
            let suffix = words(&lower[i..]);
            !suffix.is_empty()
                && marked(&suffix)
                && suffix.iter().all(|w| SUFFIX_WORDS.contains(&w.as_str()))
        })
}

/// A word matched with a small edit distance allowance that grows with its length.
#[derive(Clone, Debug)]
struct Token {
    chars: Vec<char>,
    max_distance: usize,
}

impl Token {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let max_distance = match chars.len() {
            n if n >= 7 => 2,
            n if n >= 4 => 1,
            _ => 0,
        };
        Self {
            chars,
            max_distance,
        }
    }

    fn matches(&self, other: &str) -> bool {
        let other: Vec<char> = other.chars().collect();
        if other == self.chars {
            return true;
        }
        self.max_distance > 0
            && other.len().abs_diff(self.chars.len()) <= self.max_distance
            && within_distance(&self.chars, &other, self.max_distance)
    }
}

/// Levenshtein distance between `a` and `b` is at most `max`.
fn within_distance(a: &[char], b: &[char], max: usize) -> bool {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        if cur.iter().min().is_some_and(|&m| m > max) {
            return false;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] <= max
}

/// Scores how well a candidate's title and artist match a song.
#[derive(Clone, Debug)]
pub struct Scorer {
    title: Vec<Token>,
    artist: Vec<Token>,
    /// The title's words joined, so "Nightcall" matches "Night Call".
    joined_title: Token,
    instrumental: bool,
}

impl Scorer {
    /// `title` drops bracketed notes, featured artists and filler such as "official video";
    /// `artist` drops featured artists and bracketed notes.
    pub fn new(title: &str, artist: &str) -> Self {
        let featless = strip_features(title);
        let mut title_words = words(&strip_brackets(&featless));
        if title_words.is_empty() {
            title_words = words(&featless);
        }
        let kept: Vec<String> = title_words
            .iter()
            .filter(|w| !TITLE_NOISE.contains(&w.as_str()))
            .cloned()
            .collect();
        if !kept.is_empty() {
            title_words = kept;
        }
        let artist_words = words(&strip_brackets(&strip_features(artist)));
        Self {
            joined_title: Token::new(&title_words.concat()),
            title: title_words.iter().map(|w| Token::new(w)).collect(),
            artist: artist_words.iter().map(|w| Token::new(w)).collect(),
            instrumental: is_instrumental_title(title),
        }
    }

    /// 0 to 1: title coverage weighs 0.7 and artist coverage 0.3; without an artist on
    /// either side, title coverage alone. Instrumental versions of a vocal song score lower.
    pub fn score(&self, title: &str, artist: &str) -> f32 {
        let title_parts = words(&strip_features(title));
        let artist_parts = words(&strip_features(artist));
        let all_parts: Vec<String> = title_parts.iter().chain(&artist_parts).cloned().collect();

        let mut title_score = 1.0;
        if !self.title.is_empty() {
            title_score = coverage(&self.title, &all_parts);
            if title_score < 1.0
                && (title_parts.len() > 1 || self.title.len() > 1)
                && self.joined_title.matches(&title_parts.concat())
            {
                title_score = 1.0;
            }
        }
        let mut score = if self.artist.is_empty() || artist_parts.is_empty() {
            title_score
        } else {
            // Either side may credit extra artists: "A & B" against "B".
            let all_tokens: Vec<Token> = self.title.iter().chain(&self.artist).cloned().collect();
            let forward = coverage(&self.artist, &all_parts);
            let reverse = reverse_coverage(&all_tokens, &artist_parts);
            title_score * 0.7 + forward.max(reverse) * 0.3
        };
        if !self.instrumental && is_instrumental_title(title) {
            score *= INSTRUMENTAL_PENALTY;
        }
        score
    }
}

/// Fraction of `tokens` found in `parts`.
fn coverage(tokens: &[Token], parts: &[String]) -> f32 {
    if parts.is_empty() || tokens.is_empty() {
        return 0.0;
    }
    let matched = tokens
        .iter()
        .filter(|t| parts.iter().any(|p| t.matches(p)))
        .count();
    matched as f32 / tokens.len() as f32
}

/// Fraction of `parts` matched by any of `tokens`.
fn reverse_coverage(tokens: &[Token], parts: &[String]) -> f32 {
    if parts.is_empty() {
        return 0.0;
    }
    let matched = parts
        .iter()
        .filter(|p| tokens.iter().any(|t| t.matches(p)))
        .count();
    matched as f32 / parts.len() as f32
}

// ---------------------------------------------------------------------------------------------
// Background searcher

/// State of a [`LyricsSearcher`].
#[derive(Clone, Debug, Default)]
pub enum SearchState {
    #[default]
    Idle,
    /// The search with this token is running.
    Searching(u64),
    /// The search with this token finished.
    Done(u64, Arc<SearchOutcome>),
}

#[derive(Default)]
struct SearcherInner {
    pending: Option<(u64, SearchRequest)>,
    state: SearchState,
    shutdown: bool,
}

struct SearcherShared {
    inner: Mutex<SearcherInner>,
    wake: Condvar,
    /// Token of the newest search; older searches stop issuing requests.
    latest: AtomicU64,
}

impl SearcherShared {
    fn lock(&self) -> MutexGuard<'_, SearcherInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Runs manual searches on a background thread. Only the newest search's outcome is kept:
/// starting a search or cancelling makes older ones stop early and discards their results.
pub struct LyricsSearcher {
    shared: Arc<SearcherShared>,
}

impl LyricsSearcher {
    /// `on_ready` is called from the search thread when a search finishes.
    pub fn new(on_ready: Box<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(SearcherShared {
            inner: Mutex::new(SearcherInner::default()),
            wake: Condvar::new(),
            latest: AtomicU64::new(0),
        });
        let worker = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("lyrics-search".into())
            .spawn(move || search_loop(&worker, on_ready));
        if let Err(e) = spawned {
            log::warn!("failed to start lyrics search worker: {e}");
        }
        Self { shared }
    }

    /// Starts a search, replacing any running one, and returns its token.
    pub fn search(&self, req: SearchRequest) -> u64 {
        let token = self.shared.latest.fetch_add(1, Ordering::SeqCst) + 1;
        let mut inner = self.shared.lock();
        inner.pending = Some((token, req));
        inner.state = SearchState::Searching(token);
        self.shared.wake.notify_one();
        token
    }

    /// Stops the running search and returns to [`SearchState::Idle`].
    pub fn cancel(&self) {
        self.shared.latest.fetch_add(1, Ordering::SeqCst);
        let mut inner = self.shared.lock();
        inner.pending = None;
        inner.state = SearchState::Idle;
    }

    pub fn state(&self) -> SearchState {
        self.shared.lock().state.clone()
    }
}

impl Drop for LyricsSearcher {
    fn drop(&mut self) {
        self.shared.latest.fetch_add(1, Ordering::SeqCst);
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

fn search_loop(shared: &SearcherShared, on_ready: Box<dyn Fn() + Send + Sync>) {
    let agent = new_agent();
    loop {
        let (token, req) = {
            let mut inner = shared.lock();
            loop {
                if inner.shutdown {
                    return;
                }
                if let Some(p) = inner.pending.take() {
                    break p;
                }
                inner = shared.wake.wait(inner).unwrap_or_else(|e| e.into_inner());
            }
        };
        let stale = || shared.latest.load(Ordering::SeqCst) != token;
        let outcome = search_with(&agent, &req, &stale);
        let mut inner = shared.lock();
        if !stale() {
            inner.state = SearchState::Done(token, Arc::new(outcome));
            drop(inner);
            on_ready();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Option<Duration> {
        Some(Duration::from_secs(s))
    }

    #[test]
    fn feature_and_bracket_stripping() {
        assert_eq!(strip_features("Song (feat. X)"), "song ");
        assert_eq!(strip_features("Song [Ft. X] (Live)"), "song  (live)");
        assert_eq!(strip_features("A feat. B"), "a");
        assert_eq!(strip_features("Song (With Y)"), "song ");
        assert_eq!(strip_features("Feature Story"), "feature story");
        assert_eq!(strip_brackets("Song (Remix) [2011]"), "Song  ");
        assert_eq!(words("Don't Stop—Me, Now!"), ["dont", "stop", "me", "now"]);
        assert_eq!(words("夜に駆ける"), ["夜に駆ける"]);
    }

    #[test]
    fn instrumental_titles() {
        assert!(is_instrumental_title("Song (Instrumental)"));
        assert!(is_instrumental_title("Song [Off Vocal Ver.]"));
        assert!(is_instrumental_title("Song - Instrumental"));
        assert!(is_instrumental_title("Song -Inst.-"));
        assert!(is_instrumental_title("Song (Karaoke Version)"));
        assert!(!is_instrumental_title("Song"));
        assert!(!is_instrumental_title("Artist - Instrumental Dreams"));
        assert!(!is_instrumental_title("Instrumental"));
    }

    #[test]
    fn fuzzy_tokens() {
        assert!(Token::new("abc").matches("abc"));
        assert!(!Token::new("abc").matches("abd"));
        assert!(!Token::new("night").matches("nite"));
        assert!(Token::new("lamar").matches("lamarr"));
        assert!(Token::new("beautiful").matches("beatiful"));
        assert!(!Token::new("beautiful").matches("bountiful"));
        assert!(within_distance(&['a', 'b'], &['b', 'a'], 2));
        assert!(!within_distance(&['a', 'b'], &['b', 'a'], 1));
    }

    #[test]
    fn scorer_ranking() {
        let s = Scorer::new(
            "Rich Spirit (Official Video)",
            "Kendrick Lamar feat. Someone",
        );
        let exact = s.score("Rich Spirit", "Kendrick Lamar");
        assert!(exact >= SURE_SCORE, "{exact}");
        assert!(s.score("Rich Spirit (Explicit)", "Kendrick Lamar") >= SURE_SCORE);
        // Artist folded into the title, as LRCLIB's user uploads often are.
        assert!(s.score("Kendrick Lamar - Rich Spirit", "Kendrick Lamar") >= SURE_SCORE);
        // Same title by someone else: plausible but not sure.
        let other = s.score("Rich Spirit", "Some Cover Band");
        assert!((ACCEPT_SCORE..SURE_SCORE).contains(&other), "{other}");
        // Different song.
        assert!(s.score("Count Me Out", "Kendrick Lamar") < ACCEPT_SCORE);
        // Instrumental versions rank below the vocal one.
        assert!(s.score("Rich Spirit (Instrumental)", "Kendrick Lamar") < exact);
        // Typos and word splits.
        assert!(s.score("Rich Spirt", "Kendrik Lamar") >= SURE_SCORE);
        let s = Scorer::new("Nightcall", "Kavinsky");
        assert!(s.score("Night Call", "Kavinsky") >= SURE_SCORE);
        // Extra or fewer credited artists.
        let s = Scorer::new("Duet", "A & B");
        assert!(s.score("Duet", "B") >= SURE_SCORE);
        // A custom query scores against all of its words.
        let s = Scorer::new("kendrick rich spirit", "");
        assert!(s.score("Rich Spirit", "Kendrick Lamar") >= SURE_SCORE);
        assert!(s.score("Rich Spirit", "Someone") < ACCEPT_SCORE);
    }

    const LRCLIB_SEARCH: &str = r#"[
        {"id": 1, "trackName": "Rich Spirit (Instrumental)", "artistName": "Kendrick Lamar",
         "albumName": "Mr. Morale", "duration": 202, "instrumental": true,
         "plainLyrics": null, "syncedLyrics": null},
        {"id": 2, "trackName": "Kendrick Lamar - Rich Spirit", "artistName": "Kendrick Lamar",
         "albumName": "Videos", "duration": 200.0, "instrumental": false,
         "plainLyrics": "Takin' my baby to school", "syncedLyrics": null},
        {"id": 3, "trackName": "Rich Spirit", "artistName": "Kendrick Lamar",
         "albumName": "Mr. Morale", "duration": 202.3, "instrumental": false,
         "plainLyrics": "Takin' my baby to school",
         "syncedLyrics": "[00:06.12] Takin' my baby to school"},
        {"id": 4, "trackName": "Rich Spirit", "artistName": "Kendrick Lamar",
         "albumName": "Mr. Morale (Deluxe)", "duration": 202, "instrumental": false,
         "plainLyrics": "Takin' my baby to school",
         "syncedLyrics": "[00:06.12] Takin' my baby to school"},
        {"id": 5, "trackName": "Rich Spirit", "artistName": "Cover Band",
         "albumName": null, "duration": null, "instrumental": false,
         "plainLyrics": null, "syncedLyrics": "[00:01.00]a cover"}
    ]"#;

    #[test]
    fn lrclib_parsing_and_ranking() {
        let tracks: Vec<LrclibTrack> = serde_json::from_str(LRCLIB_SEARCH).unwrap();
        assert_eq!(tracks.len(), 5);
        let scorer = Scorer::new("Rich Spirit", "Kendrick Lamar");
        let found = lrclib_found(&tracks, &scorer, secs(203));
        assert!(!found.instrumental);
        let c = &found.candidates;
        // The instrumental record has no lyrics; the duplicate synced record is dropped.
        assert_eq!(c.len(), 3);
        assert!(c[0].synced() && c[0].album == "Mr. Morale");
        assert_eq!(c[0].duration_diff, Some(Duration::from_millis(700)));
        assert!(c[0].text.starts_with(
            "[ar:Kendrick Lamar]\n[al:Mr. Morale]\n[ti:Rich Spirit]\n[length:03:22.30]\n"
        ));
        assert!(!c[1].synced() && c[1].text == "Takin' my baby to school");
        assert_eq!(c[2].artist, "Cover Band");
        assert_eq!(c[2].duration_diff, None);
        assert_eq!(best_synced(c).map(|c| c.album.as_str()), Some("Mr. Morale"));

        // A sure instrumental match is reported.
        let instrumental: Vec<LrclibTrack> = serde_json::from_str(
            r#"[{"trackName": "Rich Spirit", "artistName": "Kendrick Lamar", "duration": 202,
                 "instrumental": true}]"#,
        )
        .unwrap();
        let found = lrclib_found(&instrumental, &scorer, secs(202));
        assert!(found.instrumental && found.candidates.is_empty());
    }

    #[test]
    fn best_synced_requires_duration_and_score() {
        let tracks: Vec<LrclibTrack> = serde_json::from_str(LRCLIB_SEARCH).unwrap();
        let scorer = Scorer::new("Rich Spirit", "Kendrick Lamar");
        let found = lrclib_found(&tracks, &scorer, secs(260));
        assert!(best_synced(&found.candidates).is_none());
        let scorer = Scorer::new("Other Song", "Kendrick Lamar");
        let found = lrclib_found(&tracks, &scorer, secs(202));
        assert!(best_synced(&found.candidates).is_none());
    }

    #[test]
    fn kugou_parsing() {
        let search: KugouSearch = serde_json::from_str(
            r#"{"status":200,"candidates":[
                {"id":"11","accesskey":"AA","singer":"Kendrick Lamar","song":"Rich Spirit","duration":202318},
                {"id":"12","accesskey":"BB","singer":"Kendrick Lamar","song":"Rich Spirit","duration":202318},
                {"id":13,"accesskey":"CC","singer":"Someone","song":"Rich Spirit (Live)","duration":250000},
                {"id":"","accesskey":"DD","singer":"x","song":"y","duration":1}
            ]}"#,
        )
        .unwrap();
        let scorer = Scorer::new("Rich Spirit", "Kendrick Lamar");
        let ranked = rank_kugou(search.candidates, &scorer, secs(202));
        let ids: Vec<String> = ranked.iter().filter_map(|(_, c)| c.id()).collect();
        assert_eq!(ids, ["11", "13"]);

        let lrc = "[ti:Rich Spirit]\r\n[ar:Kendrick Lamar]\r\n[al:Mr. Morale]\r\n[00:06.56]Takin' my baby\r\n";
        let download = KugouDownload {
            content: base64::engine::general_purpose::STANDARD.encode(lrc),
        };
        let c = kugou_candidate(&ranked[0].1, &download, ranked[0].0, secs(202)).unwrap();
        assert_eq!(c.provider, Provider::Kugou);
        assert!(c.synced());
        assert_eq!(c.album, "Mr. Morale");
        assert_eq!(
            c.text,
            "[length:03:22.318]\n[ti:Rich Spirit]\n[ar:Kendrick Lamar]\n[al:Mr. Morale]\n[00:06.56]Takin' my baby"
        );
        assert!(c.is_sure_match() && c.duration_matches());

        let empty: KugouSearch = serde_json::from_str(r#"{"status":200,"candidates":[]}"#).unwrap();
        assert!(empty.candidates.is_empty());
        let bad = KugouDownload {
            content: "not base64!".into(),
        };
        assert!(kugou_candidate(&ranked[0].1, &bad, 1.0, None).is_none());
    }

    #[test]
    fn merged_ranking_dedups_identical_lyrics() {
        let make = |provider, score: f32, lrc: &str, diff: Option<u64>| Candidate {
            provider,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            duration: None,
            text: lrc.into(),
            lyrics: Arc::new(parse_lrc(lrc)),
            score,
            duration_diff: diff.map(Duration::from_secs),
        };
        let mut c = vec![
            make(Provider::Kugou, 1.0, "[00:01.00]a", Some(0)),
            make(Provider::Lrclib, 1.0, "plain a", Some(0)),
            make(Provider::Lrclib, 1.0, "[00:01.00]a", Some(3)),
            make(Provider::Lrclib, 1.0, "[00:02.00]b", Some(1)),
            make(Provider::Lrclib, 0.5, "[00:03.00]c", Some(0)),
        ];
        rank(&mut c);
        dedup(&mut c);
        let order: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            order,
            ["[00:01.00]a", "[00:02.00]b", "plain a", "[00:03.00]c"]
        );
        assert_eq!(c[0].provider, Provider::Kugou);
    }

    #[test]
    fn request_from_key() {
        let key = SongKey {
            file: "Artist/01 Song.flac".into(),
            artist: " A feat. B ".into(),
            title: String::new(),
            album: "Album".into(),
            duration: secs(100),
        };
        let req = SearchRequest::from_key(&key);
        assert_eq!(req.title, "01 Song");
        assert_eq!(req.query_artist(), "A");
        assert_eq!(req.custom_query(), None);
        let req = SearchRequest {
            query: Some("  ".into()),
            ..req
        };
        assert_eq!(req.custom_query(), None);
    }

    #[test]
    #[ignore = "requires network"]
    fn search_live() {
        let req = SearchRequest {
            title: "Rich Spirit".into(),
            artist: "Kendrick Lamar".into(),
            album: String::new(),
            duration: secs(202),
            query: None,
        };
        let outcome = search(&new_agent(), &req);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert!(outcome
            .candidates
            .iter()
            .any(|c| c.provider == Provider::Lrclib));
        assert!(outcome
            .candidates
            .iter()
            .any(|c| c.provider == Provider::Kugou));
        assert!(outcome.candidates[0].is_sure_match());
    }
}
