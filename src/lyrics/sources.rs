//! Lyrics sources: sidecar files, a local folder, the on-disk cache and LRCLIB.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use super::lrc::{parse_lrc, plain_from_text, to_lrc};
use super::{Lyrics, LyricsConfig, SongKey};

const LRCLIB_BASE: &str = "https://lrclib.net/api";
const USER_AGENT: &str = concat!(
    "mpd-display-rs/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/allylikesu/mpd-display-rs)"
);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a `.notfound` marker suppresses online lookups.
const NOTFOUND_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Maximum duration mismatch for a search result's synced lyrics.
const DURATION_TOLERANCE_SECS: f64 = 5.0;
const MAX_KEY_BYTES: usize = 150;

/// Outcome of a full lookup.
#[derive(Debug, PartialEq)]
pub enum Lookup {
    Found(Lyrics),
    /// No lyrics exist in any enabled source.
    NotFound,
    /// No local hit and the online lookup failed transiently.
    Failed,
}

/// Runs the lookup chain: sidecar, local folder, cache, then LRCLIB when `agent` is given
/// and `config.online` is set. Online hits are written to the cache.
pub fn lookup(config: &LyricsConfig, key: &SongKey, agent: Option<&ureq::Agent>) -> Lookup {
    if let Some(l) = config
        .music_dir
        .as_deref()
        .and_then(|d| sidecar(d, &key.file))
    {
        return Lookup::Found(l);
    }
    if let Some(l) = local(&config.local_dir, key) {
        return Lookup::Found(l);
    }
    let cache_dir = config.cache_dir.as_deref();
    if let Some(dir) = cache_dir {
        match read_cache(dir, key) {
            CacheEntry::Hit(l) => return Lookup::Found(l),
            CacheEntry::NotFound => return Lookup::NotFound,
            CacheEntry::Miss => {}
        }
    }
    let Some(agent) = agent.filter(|_| config.online) else {
        return Lookup::NotFound;
    };
    match fetch_lrclib(agent, key) {
        Ok(Some(l)) => {
            if let Some(dir) = cache_dir {
                write_cache(dir, key, &l);
            }
            Lookup::Found(l)
        }
        Ok(None) => {
            if let Some(dir) = cache_dir {
                write_notfound(dir, key);
            }
            Lookup::NotFound
        }
        Err(e) => {
            log::warn!("LRCLIB lookup failed for {:?}: {e}", key.file);
            Lookup::Failed
        }
    }
}

/// HTTP agent for LRCLIB requests.
pub fn new_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .new_agent()
}

/// First artist of a multi-artist credit.
pub fn primary_artist(artist: &str) -> String {
    const SEPARATORS: [&str; 7] = [", ", "/", " & ", "; ", " feat. ", " ft. ", " x "];
    // ASCII lowercasing keeps byte offsets aligned with `artist`.
    let lower = artist.to_ascii_lowercase();
    let cut = SEPARATORS
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim().to_string()
}

/// The song title, or the file stem when the title is empty.
pub fn derive_title(key: &SongKey) -> String {
    let title = key.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    Path::new(&key.file)
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default()
}

/// Filesystem-safe, lowercase `"{primary artist} - {title}"`.
pub fn cache_key(key: &SongKey) -> String {
    let artist = primary_artist(&key.artist);
    let title = derive_title(key);
    let raw = if artist.is_empty() {
        title
    } else {
        format!("{artist} - {title}")
    };
    sanitize(&raw.to_lowercase())
}

fn sanitize(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut out = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    if out.len() > MAX_KEY_BYTES {
        let mut end = MAX_KEY_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.truncate(out.trim_end().len());
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn read_text(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!("failed to read {}: {e}", path.display());
            None
        }
    }
}

fn parse_file(path: &Path) -> Option<Lyrics> {
    let text = read_text(path)?;
    let is_txt = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    let lyrics = if is_txt {
        plain_from_text(&text)
    } else {
        parse_lrc(&text)
    };
    (!lyrics.is_empty()).then_some(lyrics)
}

/// `<music_dir>/<file>.lrc`, then `.txt` as plain lyrics.
fn sidecar(music_dir: &Path, file: &str) -> Option<Lyrics> {
    if file.is_empty() || file.contains("://") {
        return None;
    }
    let base = music_dir.join(file);
    ["lrc", "txt"]
        .iter()
        .find_map(|ext| parse_file(&base.with_extension(ext)))
}

/// `<local_dir>/<artist> - <title>.lrc` (or `.txt`), matched case-insensitively; the
/// primary artist and a bare title are tried as fallbacks.
fn local(dir: &Path, key: &SongKey) -> Option<Lyrics> {
    let title = derive_title(key);
    if title.is_empty() {
        return None;
    }
    let artist = key.artist.trim();
    let primary = primary_artist(artist);
    let mut stems = Vec::new();
    for a in [artist, primary.as_str()] {
        if !a.is_empty() {
            stems.push(format!("{a} - {title}").to_lowercase());
        }
    }
    stems.push(title.to_lowercase());
    stems.dedup();

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("failed to read {}: {e}", dir.display());
            return None;
        }
    };
    let files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    for stem in &stems {
        for ext in ["lrc", "txt"] {
            let hit = files.iter().find(|p| {
                p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
                    && p.file_stem()
                        .is_some_and(|s| s.to_string_lossy().to_lowercase() == *stem)
            });
            if let Some(l) = hit.and_then(|p| parse_file(p)) {
                return Some(l);
            }
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum CacheEntry {
    Hit(Lyrics),
    /// Fresh not-found marker.
    NotFound,
    Miss,
}

fn read_cache(dir: &Path, key: &SongKey) -> CacheEntry {
    let name = cache_key(key);
    if let Some(l) = parse_file(&dir.join(format!("{name}.lrc"))) {
        return CacheEntry::Hit(l);
    }
    let marker = dir.join(format!("{name}.notfound"));
    let fresh = fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < NOTFOUND_TTL);
    if fresh {
        CacheEntry::NotFound
    } else {
        CacheEntry::Miss
    }
}

fn write_cache(dir: &Path, key: &SongKey, lyrics: &Lyrics) {
    let name = cache_key(key);
    let result = fs::create_dir_all(dir)
        .and_then(|_| fs::write(dir.join(format!("{name}.lrc")), to_lrc(lyrics)));
    if let Err(e) = result {
        log::warn!("failed to write lyrics cache in {}: {e}", dir.display());
    }
    let _ = fs::remove_file(dir.join(format!("{name}.notfound")));
}

fn write_notfound(dir: &Path, key: &SongKey) {
    let path = dir.join(format!("{}.notfound", cache_key(key)));
    if let Err(e) = fs::create_dir_all(dir).and_then(|_| fs::write(&path, b"")) {
        log::warn!("failed to write {}: {e}", path.display());
    }
}

/// A track record from the LRCLIB API.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LrclibTrack {
    #[serde(default)]
    pub synced_lyrics: Option<String>,
    #[serde(default)]
    pub plain_lyrics: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub duration: f64,
}

impl LrclibTrack {
    fn has_synced(&self) -> bool {
        !self.instrumental
            && self
                .synced_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    fn has_plain(&self) -> bool {
        !self.instrumental
            && self
                .plain_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    }

    fn synced(&self) -> Option<Lyrics> {
        self.has_synced()
            .then(|| parse_lrc(self.synced_lyrics.as_deref().unwrap_or_default()))
            .filter(|l| !l.is_empty())
    }

    fn plain(&self) -> Option<Lyrics> {
        self.has_plain()
            .then(|| plain_from_text(self.plain_lyrics.as_deref().unwrap_or_default()))
            .filter(|l| !l.is_empty())
    }
}

/// Synced lyrics if present, else plain. None for instrumentals or empty records.
pub fn lyrics_from_track(track: &LrclibTrack) -> Option<Lyrics> {
    track.synced().or_else(|| track.plain())
}

/// Best search result: among tracks within ±5s of `duration` (all tracks when unknown),
/// prefer synced lyrics, then the closest duration. When no track is within tolerance,
/// plain lyrics from the closest track are used since their timing is irrelevant.
pub fn pick_from_search(results: &[LrclibTrack], duration: Option<u64>) -> Option<Lyrics> {
    let distance = |t: &LrclibTrack| duration.map_or(0.0, |d| (t.duration - d as f64).abs());
    let in_tolerance = |t: &&LrclibTrack| distance(t) <= DURATION_TOLERANCE_SECS;
    let closest = |a: &&LrclibTrack, b: &&LrclibTrack| distance(a).total_cmp(&distance(b));

    let mut candidates: Vec<&LrclibTrack> = results
        .iter()
        .filter(in_tolerance)
        .filter(|t| t.has_synced() || t.has_plain())
        .collect();
    candidates.sort_by(|a, b| b.has_synced().cmp(&a.has_synced()).then(closest(a, b)));
    if let Some(l) = candidates.iter().find_map(|t| lyrics_from_track(t)) {
        return Some(l);
    }

    let mut rest: Vec<&LrclibTrack> = results.iter().filter(|t| t.has_plain()).collect();
    rest.sort_by(closest);
    rest.iter().find_map(|t| t.plain())
}

/// Queries LRCLIB with the full artist, then the primary artist. `Ok(None)` is a
/// definitive miss; `Err` is a transient failure.
fn fetch_lrclib(agent: &ureq::Agent, key: &SongKey) -> Result<Option<Lyrics>, ureq::Error> {
    let title = derive_title(key);
    if title.is_empty() {
        return Ok(None);
    }
    let artist = key.artist.trim();
    let primary = primary_artist(artist);
    let mut artists = vec![artist];
    if !primary.is_empty() && primary != artist {
        artists.push(&primary);
    }
    for a in artists {
        if let Some(l) = lrclib_get(agent, a, &title, key)? {
            return Ok(Some(l));
        }
        if let Some(l) = lrclib_search(agent, a, &title, key.duration_secs)? {
            return Ok(Some(l));
        }
    }
    Ok(None)
}

fn lrclib_get(
    agent: &ureq::Agent,
    artist: &str,
    title: &str,
    key: &SongKey,
) -> Result<Option<Lyrics>, ureq::Error> {
    let mut req = agent
        .get(format!("{LRCLIB_BASE}/get"))
        .query("artist_name", artist)
        .query("track_name", title);
    let album = key.album.trim();
    if !album.is_empty() {
        req = req.query("album_name", album);
    }
    if let Some(d) = key.duration_secs {
        req = req.query("duration", d.to_string());
    }
    match req.call() {
        Ok(mut resp) => Ok(lyrics_from_track(
            &resp.body_mut().read_json::<LrclibTrack>()?,
        )),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn lrclib_search(
    agent: &ureq::Agent,
    artist: &str,
    title: &str,
    duration: Option<u64>,
) -> Result<Option<Lyrics>, ureq::Error> {
    let mut req = agent
        .get(format!("{LRCLIB_BASE}/search"))
        .query("track_name", title);
    if !artist.is_empty() {
        req = req.query("artist_name", artist);
    }
    match req.call() {
        Ok(mut resp) => {
            let results = resp.body_mut().read_json::<Vec<LrclibTrack>>()?;
            Ok(pick_from_search(&results, duration))
        }
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mpd-display-rs-test-{tag}-{}-{nanos}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(file: &str, artist: &str, title: &str) -> SongKey {
        SongKey {
            file: file.into(),
            artist: artist.into(),
            title: title.into(),
            album: String::new(),
            duration_secs: None,
        }
    }

    fn config(dir: &Path) -> LyricsConfig {
        LyricsConfig {
            music_dir: Some(dir.join("music")),
            local_dir: dir.join("local"),
            cache_dir: Some(dir.join("cache")),
            online: false,
        }
    }

    fn track(synced: Option<&str>, plain: Option<&str>, duration: f64) -> LrclibTrack {
        LrclibTrack {
            synced_lyrics: synced.map(Into::into),
            plain_lyrics: plain.map(Into::into),
            instrumental: false,
            duration,
        }
    }

    #[test]
    fn primary_artist_splitting() {
        assert_eq!(primary_artist("Solo"), "Solo");
        assert_eq!(primary_artist("A, B"), "A");
        assert_eq!(primary_artist("A/B"), "A");
        assert_eq!(primary_artist("A & B"), "A");
        assert_eq!(primary_artist("A; B"), "A");
        assert_eq!(primary_artist("A feat. B"), "A");
        assert_eq!(primary_artist("A FEAT. B"), "A");
        assert_eq!(primary_artist("A Ft. B, C"), "A");
        assert_eq!(primary_artist("A x B"), "A");
        assert_eq!(primary_artist("Xavier"), "Xavier");
        assert_eq!(primary_artist("Céline Dion & X"), "Céline Dion");
        assert_eq!(primary_artist(""), "");
    }

    #[test]
    fn derive_title_falls_back_to_stem() {
        assert_eq!(derive_title(&key("a/b.flac", "", " T ")), "T");
        assert_eq!(
            derive_title(&key("Artist/01 - Song.flac", "", "")),
            "01 - Song"
        );
        assert_eq!(derive_title(&key("", "", "")), "");
    }

    #[test]
    fn cache_key_sanitized() {
        assert_eq!(cache_key(&key("f", "A feat. B", "Song")), "a - song");
        assert_eq!(
            cache_key(&key("f", "AC/DC", "What?  Is: <this>\t|now|")),
            "ac - what_ is_ _this___now_"
        );
        assert_eq!(cache_key(&key("x/.hidden.mp3", "", "")), "_hidden");
        assert_eq!(cache_key(&key("", "", "")), "_");
        let long = cache_key(&key("f", "", &"é".repeat(200)));
        assert!(long.len() <= MAX_KEY_BYTES);
        assert!(long.chars().all(|c| c == 'é'));
    }

    const GET_FIXTURE: &str = r#"{
        "id": 3396226, "trackName": "I Want to Live", "artistName": "Borislav Slavov",
        "albumName": "Baldur's Gate 3", "duration": 233, "instrumental": false,
        "plainLyrics": "I feel your breath\nUpon my neck",
        "syncedLyrics": "[00:17.12] I feel your breath\n[00:19.18] Upon my neck"
    }"#;

    #[test]
    fn lrclib_fixture_parsing() {
        let t: LrclibTrack = serde_json::from_str(GET_FIXTURE).unwrap();
        assert_eq!(t.duration, 233.0);
        let l = lyrics_from_track(&t).unwrap();
        assert!(
            matches!(&l, Lyrics::Synced(v) if v.len() == 2 && v[0].text == "I feel your breath")
        );

        let t: LrclibTrack = serde_json::from_str(
            r#"{"duration": 100.5, "instrumental": false, "plainLyrics": "a\nb", "syncedLyrics": null}"#,
        )
        .unwrap();
        assert_eq!(
            lyrics_from_track(&t),
            Some(Lyrics::Plain(vec!["a".into(), "b".into()]))
        );

        let t: LrclibTrack = serde_json::from_str(
            r#"{"duration": 100, "instrumental": true, "plainLyrics": null, "syncedLyrics": null}"#,
        )
        .unwrap();
        assert_eq!(lyrics_from_track(&t), None);

        let list: Vec<LrclibTrack> = serde_json::from_str(&format!("[{GET_FIXTURE}]")).unwrap();
        assert!(pick_from_search(&list, Some(234)).is_some());
    }

    #[test]
    fn search_prefers_synced_then_closest() {
        let results = vec![
            track(None, Some("plain close"), 200.0),
            track(Some("[00:01.00]synced far"), None, 204.0),
            track(Some("[00:01.00]synced near"), None, 201.0),
            track(Some("[00:01.00]synced out"), None, 230.0),
        ];
        let text = |l: Option<Lyrics>| match l.unwrap() {
            Lyrics::Synced(v) => v[0].text.clone(),
            Lyrics::Plain(v) => v[0].clone(),
        };
        assert_eq!(text(pick_from_search(&results, Some(200))), "synced near");
        assert_eq!(text(pick_from_search(&results, Some(230))), "synced out");
        // Unknown duration: first synced result.
        assert_eq!(text(pick_from_search(&results, None)), "synced far");
        // Nothing in tolerance: closest plain lyrics.
        assert_eq!(text(pick_from_search(&results, Some(100))), "plain close");
        let synced_only = &results[1..];
        assert_eq!(pick_from_search(synced_only, Some(100)), None);
        assert_eq!(pick_from_search(&[], Some(100)), None);
        let instrumental = LrclibTrack {
            instrumental: true,
            ..track(Some("[00:01.00]x"), Some("x"), 200.0)
        };
        assert_eq!(pick_from_search(&[instrumental], Some(200)), None);
    }

    #[test]
    fn sidecar_lookup() {
        let dir = temp_dir("sidecar");
        let cfg = config(&dir);
        let album = dir.join("music/Artist/Album");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Song.lrc"), "[00:01.00]synced").unwrap();
        fs::write(album.join("02 Other.txt"), "plain one\nplain two\n").unwrap();

        let k = key("Artist/Album/01 Song.flac", "Artist", "Song");
        assert!(matches!(
            lookup(&cfg, &k, None),
            Lookup::Found(Lyrics::Synced(_))
        ));
        let k = key("Artist/Album/02 Other.mp3", "Artist", "Other");
        assert_eq!(
            lookup(&cfg, &k, None),
            Lookup::Found(Lyrics::Plain(vec!["plain one".into(), "plain two".into()]))
        );
        let k = key("Artist/Album/03 None.mp3", "Artist", "None");
        assert_eq!(lookup(&cfg, &k, None), Lookup::NotFound);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn local_folder_lookup() {
        let dir = temp_dir("local");
        let cfg = config(&dir);
        fs::create_dir_all(&cfg.local_dir).unwrap();
        fs::write(
            cfg.local_dir.join("The Band - Some Song.LRC"),
            "[00:02.00]x",
        )
        .unwrap();
        fs::write(cfg.local_dir.join("Lead - Duet.lrc"), "[00:03.00]y").unwrap();

        let k = key("a.flac", "the band", "some song");
        assert!(matches!(lookup(&cfg, &k, None), Lookup::Found(_)));
        let k = key("b.flac", "Lead feat. Guest", "Duet");
        assert!(matches!(lookup(&cfg, &k, None), Lookup::Found(_)));
        let k = key("c.flac", "The Band", "Missing");
        assert_eq!(lookup(&cfg, &k, None), Lookup::NotFound);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_lookup_and_notfound_marker() {
        let dir = temp_dir("cache");
        let cfg = config(&dir);
        let cache = cfg.cache_dir.clone().unwrap();
        let k = key("x.flac", "Artist", "Cached");
        assert_eq!(read_cache(&cache, &k), CacheEntry::Miss);

        let lyrics = parse_lrc("[00:01.00]one\n[00:02.50]two");
        write_cache(&cache, &k, &lyrics);
        assert!(cache.join("artist - cached.lrc").exists());
        assert_eq!(lookup(&cfg, &k, None), Lookup::Found(lyrics));

        let k = key("y.flac", "Artist", "Absent");
        write_notfound(&cache, &k);
        assert_eq!(read_cache(&cache, &k), CacheEntry::NotFound);
        assert_eq!(lookup(&cfg, &k, None), Lookup::NotFound);

        // A stale marker no longer counts.
        let marker = cache.join("artist - absent.notfound");
        let old = SystemTime::now() - NOTFOUND_TTL - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(read_cache(&cache, &k), CacheEntry::Miss);

        // Writing lyrics clears the marker.
        write_cache(&cache, &k, &Lyrics::Plain(vec!["p".into()]));
        assert!(!marker.exists());
        assert_eq!(
            read_cache(&cache, &k),
            CacheEntry::Hit(Lyrics::Plain(vec!["p".into()]))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sources_take_priority_in_order() {
        let dir = temp_dir("order");
        let cfg = config(&dir);
        let k = key("Song.flac", "Artist", "Song");
        fs::create_dir_all(dir.join("music")).unwrap();
        fs::create_dir_all(&cfg.local_dir).unwrap();
        write_cache(
            cfg.cache_dir.as_ref().unwrap(),
            &k,
            &Lyrics::Plain(vec!["cache".into()]),
        );
        fs::write(cfg.local_dir.join("Artist - Song.lrc"), "local").unwrap();
        fs::write(dir.join("music/Song.lrc"), "sidecar").unwrap();

        let first = |cfg: &LyricsConfig| match lookup(cfg, &k, None) {
            Lookup::Found(Lyrics::Plain(v)) => v[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(first(&cfg), "sidecar");
        fs::remove_file(dir.join("music/Song.lrc")).unwrap();
        assert_eq!(first(&cfg), "local");
        fs::remove_file(cfg.local_dir.join("Artist - Song.lrc")).unwrap();
        assert_eq!(first(&cfg), "cache");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "requires network"]
    fn lrclib_live() {
        let mut k = key("x.flac", "Borislav Slavov feat. Someone", "I Want to Live");
        k.duration_secs = Some(233);
        let l = fetch_lrclib(&new_agent(), &k).unwrap().unwrap();
        assert!(matches!(l, Lyrics::Synced(_)));
    }
}
