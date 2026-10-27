//! Lyrics sources: manual overrides, sidecar files, a local folder, the on-disk cache, LRCLIB
//! and KuGou.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::lrc::{decode_text, parse_lrc, plain_from_text};
use super::search::{self, LrclibTrack, SearchRequest, KUGOU_AUTO_LIMIT};
use super::{manual, Lyrics, LyricsConfig, Origin, Provider, SongKey};

pub(crate) const LRCLIB_BASE: &str = "https://lrclib.net/api";
const USER_AGENT: &str = concat!(
    "encore/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/allylikesu/mpd-display-rs)"
);
/// Limit for a whole request, including name resolution and the response body. Resolving a
/// host can take several seconds on some systems.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Limit for the TCP connection itself, so an unreachable address fails fast.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an idle connection is kept for reuse. Long enough to span a typical song, so the
/// next song's lookup skips name resolution and the TLS handshake. Connections the server
/// closed in the meantime are detected and replaced.
const MAX_IDLE_AGE: Duration = Duration::from_secs(300);
/// How long a `.notfound` marker suppresses online lookups.
const NOTFOUND_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Contents of a `.notfound` marker: the providers that found nothing. Markers with other
/// contents were written before every provider was consulted and are ignored.
const NOTFOUND_MARKER: &str = "lrclib kugou\n";
/// Cache file tag naming the provider the lyrics came from.
const SOURCE_TAG: &str = "source";
const MAX_KEY_BYTES: usize = 150;

/// Lyrics found by a lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub lyrics: Lyrics,
    pub origin: Origin,
    /// Text the lyrics were parsed from: LRC or plain text.
    pub text: String,
}

/// Outcome of a full lookup.
#[derive(Debug, PartialEq)]
pub enum Lookup {
    Found(Found),
    /// No lyrics exist in any enabled source.
    NotFound,
    /// No local hit and the online lookup failed transiently.
    Failed,
}

/// Runs the lookup chain: manual override, sidecar, local folder, cache, then the online
/// providers when `agent` is given and `config.online` is set. Online hits are written to the
/// cache.
pub fn lookup(config: &LyricsConfig, key: &SongKey, agent: Option<&ureq::Agent>) -> Lookup {
    let cache_dir = config.cache_dir.as_deref();
    if let Some((lyrics, text)) = cache_dir.and_then(|d| manual::read_override(d, key)) {
        return found(lyrics, Origin::Manual, text);
    }
    if let Some((lyrics, text)) = sidecar(config.music_dir.as_deref(), &key.file) {
        return found(lyrics, Origin::Sidecar, text);
    }
    if let Some((lyrics, text)) = local(&config.local_dir, key) {
        return found(lyrics, Origin::Local, text);
    }
    if let Some(dir) = cache_dir {
        match read_cache(dir, key) {
            CacheEntry::Hit(found) => return Lookup::Found(found),
            CacheEntry::NotFound => return Lookup::NotFound,
            CacheEntry::Miss => {}
        }
    }
    let Some(agent) = agent.filter(|_| config.online) else {
        return Lookup::NotFound;
    };
    let online = fetch_online(agent, key);
    match (online.found, online.error) {
        (Some((lyrics, provider, text)), error) => {
            match (cache_dir, error) {
                (Some(dir), None) => write_cache(dir, key, provider, &text),
                // A provider failed, so a better result may exist; look again next time.
                (_, Some(e)) => log::warn!("lyrics lookup partly failed for {:?}: {e}", key.file),
                (None, None) => {}
            }
            found(lyrics, Origin::Online(provider), text)
        }
        (None, None) => {
            if let Some(dir) = cache_dir {
                write_notfound(dir, key);
            }
            Lookup::NotFound
        }
        (None, Some(e)) => {
            log::warn!("lyrics lookup failed for {:?}: {e}", key.file);
            Lookup::Failed
        }
    }
}

fn found(lyrics: Lyrics, origin: Origin, text: String) -> Lookup {
    Lookup::Found(Found {
        lyrics,
        origin,
        text,
    })
}

/// HTTP agent for lyrics and artwork requests.
pub fn new_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .max_idle_age(MAX_IDLE_AGE)
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
        Ok(bytes) => Some(decode_text(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!("failed to read {}: {e}", path.display());
            None
        }
    }
}

/// A `.txt` file as plain lyrics, anything else as LRC, with the file text.
fn parse_file(path: &Path) -> Option<(Lyrics, String)> {
    let text = read_text(path)?;
    let is_txt = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    let lyrics = if is_txt {
        plain_from_text(&text)
    } else {
        parse_lrc(&text)
    };
    (!lyrics.is_empty()).then_some((lyrics, text))
}

/// `<file>.lrc`, then `.txt` as plain lyrics, next to the audio file: `file` itself when it is
/// an absolute path, else `file` under `music_dir`.
fn sidecar(music_dir: Option<&Path>, file: &str) -> Option<(Lyrics, String)> {
    if file.is_empty() || file.contains("://") {
        return None;
    }
    let file = Path::new(file);
    let base = if file.is_absolute() {
        file.to_path_buf()
    } else {
        music_dir?.join(file)
    };
    ["lrc", "txt"]
        .iter()
        .find_map(|ext| parse_file(&base.with_extension(ext)))
}

/// `<local_dir>/<artist> - <title>.lrc` (or `.txt`), matched case-insensitively; the
/// primary artist and a bare title are tried as fallbacks.
fn local(dir: &Path, key: &SongKey) -> Option<(Lyrics, String)> {
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
    Hit(Found),
    /// Fresh not-found marker.
    NotFound,
    Miss,
}

/// The text without `[source:]` lines.
fn strip_source_tag(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let tag = line.trim_start().to_ascii_lowercase();
        if !tag.starts_with(&format!("[{SOURCE_TAG}:")) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Cache files are LRC with a `[source:<provider>]` line. Files without timestamps are plain
/// lyrics. A file without the tag comes from LRCLIB.
fn read_cache(dir: &Path, key: &SongKey) -> CacheEntry {
    let name = cache_key(key);
    if let Some(raw) = read_text(&dir.join(format!("{name}.lrc"))) {
        let provider = super::lrc::tag_value(&raw, SOURCE_TAG)
            .and_then(Provider::from_tag)
            .unwrap_or(Provider::Lrclib);
        let text = strip_source_tag(&raw);
        let lyrics = match parse_lrc(&text) {
            l @ Lyrics::Synced(_) => l,
            Lyrics::Plain(_) => plain_from_text(&text),
        };
        if !lyrics.is_empty() {
            return CacheEntry::Hit(Found {
                lyrics,
                origin: Origin::Cache(provider),
                text,
            });
        }
    }
    let marker = dir.join(format!("{name}.notfound"));
    let fresh = fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < NOTFOUND_TTL);
    if fresh && read_text(&marker).as_deref() == Some(NOTFOUND_MARKER) {
        CacheEntry::NotFound
    } else {
        CacheEntry::Miss
    }
}

fn write_cache(dir: &Path, key: &SongKey, provider: Provider, text: &str) {
    let name = cache_key(key);
    let contents = format!("[{SOURCE_TAG}:{}]\n{}\n", provider.tag(), text.trim());
    let result =
        fs::create_dir_all(dir).and_then(|_| fs::write(dir.join(format!("{name}.lrc")), contents));
    if let Err(e) = result {
        log::warn!("failed to write lyrics cache in {}: {e}", dir.display());
    }
    let _ = fs::remove_file(dir.join(format!("{name}.notfound")));
}

fn write_notfound(dir: &Path, key: &SongKey) {
    let path = dir.join(format!("{}.notfound", cache_key(key)));
    if let Err(e) = fs::create_dir_all(dir).and_then(|_| fs::write(&path, NOTFOUND_MARKER)) {
        log::warn!("failed to write {}: {e}", path.display());
    }
}

/// Result of the online lookup: the lyrics found, if any, and the first transient error when
/// a provider failed and no synced lyrics were found.
struct Online {
    found: Option<(Lyrics, Provider, String)>,
    error: Option<ureq::Error>,
}

impl Online {
    fn done(found: Option<(Lyrics, Provider, String)>) -> Self {
        Self { found, error: None }
    }
}

/// LRCLIB's exact match, then the best synced match from LRCLIB's search, then KuGou's.
/// Plain lyrics from LRCLIB are the last resort. A sure instrumental match ends the lookup
/// without lyrics.
fn fetch_online(agent: &ureq::Agent, key: &SongKey) -> Online {
    let req = SearchRequest::from_key(key);
    if req.title.is_empty() {
        return Online::done(None);
    }
    let from_candidate = |c: &search::Candidate| ((*c.lyrics).clone(), c.provider, c.text.clone());
    let mut plain = None;
    let mut error = None;

    let primary = primary_artist(&req.artist);
    let mut artists = vec![req.artist.as_str()];
    if !primary.is_empty() && primary != req.artist {
        artists.push(&primary);
    }
    for artist in artists {
        match lrclib_get(agent, artist, &req.title, key) {
            Ok(Some(track)) if track.instrumental => return Online::done(None),
            Ok(Some(track)) => {
                if let Some(l) = track.synced() {
                    let text = search::lrclib_text(&track);
                    return Online::done(Some((l, Provider::Lrclib, text)));
                }
                if plain.is_none() {
                    plain = track
                        .plain()
                        .map(|l| (l, Provider::Lrclib, search::lrclib_text(&track)));
                }
            }
            Ok(None) => {}
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }

    let (lrclib, kugou) = search::search_providers(agent, &req, KUGOU_AUTO_LIMIT, &|| false);
    match lrclib {
        Ok(found) => {
            if found.instrumental {
                return Online::done(plain);
            }
            if let Some(c) = search::best_synced(&found.candidates) {
                return Online::done(Some(from_candidate(c)));
            }
            if plain.is_none() {
                plain = found
                    .candidates
                    .iter()
                    .find(|c| !c.synced() && c.is_sure_match())
                    .map(from_candidate);
            }
        }
        Err(e) => error = error.or(Some(e)),
    }
    match kugou {
        Ok(found) => {
            if let Some(c) = search::best_synced(&found) {
                return Online::done(Some(from_candidate(c)));
            }
        }
        Err(e) => error = error.or(Some(e)),
    }
    Online {
        found: plain,
        error,
    }
}

/// LRCLIB's exact-match endpoint. `Ok(None)` when it has no such track.
fn lrclib_get(
    agent: &ureq::Agent,
    artist: &str,
    title: &str,
    key: &SongKey,
) -> Result<Option<LrclibTrack>, ureq::Error> {
    let mut req = agent
        .get(format!("{LRCLIB_BASE}/get"))
        .query("artist_name", artist)
        .query("track_name", title);
    let album = key.album.trim();
    if !album.is_empty() {
        req = req.query("album_name", album);
    }
    if let Some(d) = key.duration {
        req = req.query("duration", format!("{:.0}", d.as_secs_f64()));
    }
    match req.call() {
        Ok(mut resp) => Ok(Some(resp.body_mut().read_json::<LrclibTrack>()?)),
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
            "encore-test-{tag}-{}-{nanos}-{}",
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
            duration: None,
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

    fn lyrics_of(lookup: Lookup) -> Lyrics {
        match lookup {
            Lookup::Found(f) => f.lyrics,
            other => panic!("{other:?}"),
        }
    }

    fn origin_of(lookup: Lookup) -> Origin {
        match lookup {
            Lookup::Found(f) => f.origin,
            other => panic!("{other:?}"),
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
        assert_eq!(t.duration, Some(233.0));
        let l = t.lyrics().unwrap();
        assert!(
            matches!(&l, Lyrics::Synced(v) if v.len() == 2 && v[0].text == "I feel your breath")
        );
        assert!(search::lrclib_text(&t).starts_with(
            "[ar:Borislav Slavov]\n[al:Baldur's Gate 3]\n[ti:I Want to Live]\n[length:03:53.00]\n"
        ));

        let t: LrclibTrack = serde_json::from_str(
            r#"{"duration": 100.5, "instrumental": false, "plainLyrics": "a\nb", "syncedLyrics": null}"#,
        )
        .unwrap();
        assert_eq!(
            t.lyrics(),
            Some(Lyrics::Plain(vec!["a".into(), "b".into()]))
        );
        assert_eq!(search::lrclib_text(&t), "a\nb");

        let t: LrclibTrack = serde_json::from_str(
            r#"{"duration": 100, "instrumental": true, "plainLyrics": null, "syncedLyrics": null}"#,
        )
        .unwrap();
        assert_eq!(t.lyrics(), None);
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
            lyrics_of(lookup(&cfg, &k, None)),
            Lyrics::Synced(_)
        ));
        assert_eq!(origin_of(lookup(&cfg, &k, None)), Origin::Sidecar);
        let k = key("Artist/Album/02 Other.mp3", "Artist", "Other");
        assert_eq!(
            lyrics_of(lookup(&cfg, &k, None)),
            Lyrics::Plain(vec!["plain one".into(), "plain two".into()])
        );
        let k = key("Artist/Album/03 None.mp3", "Artist", "None");
        assert_eq!(lookup(&cfg, &k, None), Lookup::NotFound);

        // An absolute path is searched even without a music directory.
        let no_music_dir = LyricsConfig {
            music_dir: None,
            ..cfg.clone()
        };
        let absolute = album.join("01 Song.flac");
        let k = key(&absolute.to_string_lossy(), "Artist", "Song");
        assert!(matches!(
            lyrics_of(lookup(&no_music_dir, &k, None)),
            Lyrics::Synced(_)
        ));
        let k = key("Artist/Album/01 Song.flac", "Artist", "Song");
        assert_eq!(lookup(&no_music_dir, &k, None), Lookup::NotFound);
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
        assert_eq!(origin_of(lookup(&cfg, &k, None)), Origin::Local);
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

        let text = "[length:01:00.00]\n[00:01.00]one\n[00:02.50]two";
        write_cache(&cache, &k, Provider::Kugou, text);
        assert!(cache.join("artist - cached.lrc").exists());
        let CacheEntry::Hit(hit) = read_cache(&cache, &k) else {
            panic!("expected a cache hit");
        };
        assert_eq!(hit.lyrics, parse_lrc(text));
        assert_eq!(hit.origin, Origin::Cache(Provider::Kugou));
        assert_eq!(hit.text, format!("{text}\n"));

        // Files without a source tag come from LRCLIB.
        fs::write(cache.join("artist - cached.lrc"), "[00:01.00]old").unwrap();
        assert_eq!(
            origin_of(lookup(&cfg, &k, None)),
            Origin::Cache(Provider::Lrclib)
        );

        let k = key("y.flac", "Artist", "Absent");
        write_notfound(&cache, &k);
        assert_eq!(read_cache(&cache, &k), CacheEntry::NotFound);
        assert_eq!(lookup(&cfg, &k, None), Lookup::NotFound);

        // A marker from before every provider was consulted no longer counts.
        let marker = cache.join("artist - absent.notfound");
        fs::write(&marker, "").unwrap();
        assert_eq!(read_cache(&cache, &k), CacheEntry::Miss);

        // A stale marker no longer counts.
        write_notfound(&cache, &k);
        let old = SystemTime::now() - NOTFOUND_TTL - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(read_cache(&cache, &k), CacheEntry::Miss);

        // Writing lyrics clears the marker; plain lyrics keep bracketed lines.
        write_cache(&cache, &k, Provider::Lrclib, "p\n[Chorus]");
        assert!(!marker.exists());
        let CacheEntry::Hit(hit) = read_cache(&cache, &k) else {
            panic!("expected a cache hit");
        };
        assert_eq!(
            hit.lyrics,
            Lyrics::Plain(vec!["p".into(), "[Chorus]".into()])
        );
        assert_eq!(hit.text, "p\n[Chorus]\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sources_take_priority_in_order() {
        let dir = temp_dir("order");
        let cfg = config(&dir);
        let k = key("Song.flac", "Artist", "Song");
        fs::create_dir_all(dir.join("music")).unwrap();
        fs::create_dir_all(&cfg.local_dir).unwrap();
        let cache = cfg.cache_dir.clone().unwrap();
        write_cache(&cache, &k, Provider::Lrclib, "cache");
        fs::write(cfg.local_dir.join("Artist - Song.lrc"), "local").unwrap();
        fs::write(dir.join("music/Song.lrc"), "sidecar").unwrap();
        manual::write_override(&cache, &k, "manual").unwrap();

        let first = |cfg: &LyricsConfig| match lyrics_of(lookup(cfg, &k, None)) {
            Lyrics::Plain(v) => v[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(first(&cfg), "manual");
        assert_eq!(origin_of(lookup(&cfg, &k, None)), Origin::Manual);
        manual::delete_override(&cache, &k).unwrap();
        assert_eq!(first(&cfg), "sidecar");
        fs::remove_file(dir.join("music/Song.lrc")).unwrap();
        assert_eq!(first(&cfg), "local");
        fs::remove_file(cfg.local_dir.join("Artist - Song.lrc")).unwrap();
        assert_eq!(first(&cfg), "cache");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "requires network"]
    fn online_live() {
        let mut k = key("x.flac", "Borislav Slavov feat. Someone", "I Want to Live");
        k.duration = Some(Duration::from_secs(233));
        let online = fetch_online(&new_agent(), &k);
        assert!(online.error.is_none(), "{:?}", online.error);
        let (l, provider, _) = online.found.unwrap();
        assert!(matches!(l, Lyrics::Synced(_)));
        assert_eq!(provider, Provider::Lrclib);
    }
}
