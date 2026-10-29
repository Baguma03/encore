//! Artist background images: the manual artists folder, the download cache and online lookups
//! on TheAudioDB and Deezer.
//!
//! An artist is identified by its key, the first artist of the song's artist tag in lowercase
//! (see [`first_artist`]). Lookups run in this order and the first hit wins:
//!
//! 1. `<key>.<ext>` in the manual folder ([`crate::paths::artists_dir`]),
//! 2. `<key>.<ext>` in the cache ([`crate::paths::artist_cache_dir`]), where a fresh
//!    `<key>.notfound` marker ends the lookup,
//! 3. TheAudioDB, then Deezer, when online lookups are enabled. A downloaded image is saved
//!    to the cache; when neither service has one, a not-found marker is written instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;

/// Image extensions looked for in the manual folder and the cache, in order.
pub const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

const AUDIODB_SEARCH: &str = "https://www.theaudiodb.com/api/v1/json/123/search.php?s=";
const DEEZER_SEARCH: &str = "https://api.deezer.com/search/artist?limit=25&q=";
/// TheAudioDB image fields, most suitable first: 1280x720 fan art, then the wide thumbnail.
const AUDIODB_FIELDS: [&str; 5] = [
    "strArtistFanart",
    "strArtistFanart2",
    "strArtistFanart3",
    "strArtistFanart4",
    "strArtistWideThumb",
];
/// Largest image downloaded.
const DOWNLOAD_MAX: u64 = 20 * 1024 * 1024;
/// How long a `.notfound` marker suppresses online lookups.
pub const NOTFOUND_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Delay before an artist whose lookup failed on a network error is looked up again.
const RETRY_AFTER: Duration = Duration::from_secs(60);
const MAX_KEY_BYTES: usize = 150;

/// First artist of a multi-artist tag, trimmed, in its original case.
pub fn first_artist_name(artist: &str) -> &str {
    [", ", "/", " & ", "; "]
        .iter()
        .fold(artist, |name, sep| name.split(sep).next().unwrap_or(name))
        .trim()
}

/// First artist of a multi-artist tag, lowercased to match the artist image file names.
pub fn first_artist(artist: &str) -> String {
    first_artist_name(artist).to_lowercase()
}

/// Where artist images are looked for.
#[derive(Clone, Debug)]
pub struct ArtistConfig {
    /// Folder of images added by hand; always takes precedence.
    pub manual_dir: PathBuf,
    /// Cache of downloaded images and not-found markers.
    pub cache_dir: Option<PathBuf>,
    /// Enables the TheAudioDB and Deezer lookups.
    pub online: bool,
}

impl ArtistConfig {
    /// The manual folder and cache under the XDG data and cache directories.
    pub fn standard(online: bool) -> Self {
        Self {
            manual_dir: crate::paths::artists_dir(),
            cache_dir: crate::paths::artist_cache_dir(),
            online,
        }
    }
}

/// Existing image files for the first artist of `artist`: the manual folder's, then the
/// cache's.
pub fn image_paths(manual_dir: &Path, cache_dir: Option<&Path>, artist: &str) -> Vec<PathBuf> {
    let key = first_artist(artist);
    if key.is_empty() {
        return Vec::new();
    }
    let manual = EXTENSIONS
        .iter()
        .map(|ext| manual_dir.join(format!("{key}.{ext}")));
    let cached = cache_dir
        .zip(cache_name(&key))
        .into_iter()
        .flat_map(|(dir, name)| {
            EXTENSIONS
                .iter()
                .map(move |ext| dir.join(format!("{name}.{ext}")))
        });
    manual.chain(cached).filter(|p| p.is_file()).collect()
}

/// File name stem of `key` in the cache: path separators, control characters and a leading
/// dot replaced, and long names shortened. `None` for an empty key.
fn cache_name(key: &str) -> Option<String> {
    let mut out: String = key
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    if out.len() > MAX_KEY_BYTES {
        let mut end = MAX_KEY_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    let out = out.trim().to_owned();
    (!out.is_empty()).then_some(out)
}

/// Whether a not-found marker last modified at `modified` still suppresses lookups at `now`.
/// A marker dated in the future does not.
pub fn marker_is_fresh(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age < NOTFOUND_TTL)
}

fn has_fresh_marker(dir: &Path, name: &str) -> bool {
    fs::metadata(dir.join(format!("{name}.notfound")))
        .and_then(|m| m.modified())
        .is_ok_and(|t| marker_is_fresh(t, SystemTime::now()))
}

/// `name` lowercased with common Latin diacritics removed, `&` read as "and" and other
/// punctuation dropped, so "Beyoncé" matches "Beyonce" and "Simon & Garfunkel" matches
/// "Simon and Garfunkel".
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.to_lowercase().chars() {
        match c {
            'à'..='å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'æ' => out.push_str("ae"),
            'ç' | 'ć' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è'..='ë' | 'ē' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ì'..='ï' | 'ī' | 'į' | 'ı' => out.push('i'),
            'ł' | 'ľ' => out.push('l'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò'..='ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'œ' => out.push_str("oe"),
            'ř' => out.push('r'),
            'ß' => out.push_str("ss"),
            'ś' | 'š' | 'ş' => out.push('s'),
            'ť' | 'ţ' => out.push('t'),
            'ù'..='ü' | 'ū' | 'ů' | 'ű' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            '&' => out.push_str(" and "),
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a service's artist name `found` names the queried artist `wanted`.
pub fn names_match(wanted: &str, found: &str) -> bool {
    let (a, b) = (normalize(wanted), normalize(found));
    if a.is_empty() || b.is_empty() {
        // Names of punctuation alone, like "!!!", compare as written.
        let (a, b) = (wanted.trim().to_lowercase(), found.trim().to_lowercase());
        return !a.is_empty() && a == b;
    }
    a == b
}

/// Image URLs of the matching artist in a TheAudioDB `search.php` response, most suitable
/// first. Empty when no returned artist is `artist`.
pub fn audiodb_image_urls(json: &str, artist: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(entry) = value["artists"].as_array().and_then(|list| {
        list.iter().find(|a| {
            a["strArtist"]
                .as_str()
                .is_some_and(|n| names_match(artist, n))
        })
    }) else {
        return Vec::new();
    };
    AUDIODB_FIELDS
        .iter()
        .filter_map(|field| entry[field].as_str())
        .map(str::trim)
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(str::to_owned)
        .collect()
}

/// Whether a Deezer picture URL is the grey silhouette shown for artists without a photo. Its
/// image hash is empty: `.../images/artist//1000x1000-...jpg`.
pub fn is_deezer_placeholder(url: &str) -> bool {
    match url.split_once("/images/artist/") {
        Some((_, rest)) => rest.split('/').next().is_none_or(str::is_empty),
        None => false,
    }
}

/// The 1000x1000 picture of the first artist named `artist` in a Deezer `search/artist`
/// response, unless it is the placeholder silhouette.
pub fn deezer_image_url(json: &str, artist: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(json).ok()?;
    let entry = value["data"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| names_match(artist, n)))?;
    let url = entry["picture_xl"].as_str()?.trim();
    (url.starts_with("https://") || url.starts_with("http://"))
        .then_some(url)
        .filter(|u| !is_deezer_placeholder(u))
        .map(str::to_owned)
}

/// File extension for image `bytes` that decode as a supported format.
fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    let ext = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::Png => "png",
        image::ImageFormat::WebP => "webp",
        _ => return None,
    };
    image::load_from_memory(bytes).ok().map(|_| ext)
}

/// Outcome of [`lookup`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// The manual folder or the cache already has an image.
    Present,
    /// An image was downloaded and saved to this cache file.
    Downloaded(PathBuf),
    /// No image exists in any enabled source.
    NotFound,
    /// No local image and the online lookup failed on a network error.
    Failed,
}

/// Runs the lookup chain for the first artist of `artist`: manual folder, cache, then
/// TheAudioDB and Deezer when `agent` is given and online lookups are enabled.
pub fn lookup(config: &ArtistConfig, artist: &str, agent: Option<&ureq::Agent>) -> Fetched {
    let name = first_artist_name(artist);
    let key = name.to_lowercase();
    if key.is_empty() {
        return Fetched::NotFound;
    }
    if !image_paths(&config.manual_dir, config.cache_dir.as_deref(), artist).is_empty() {
        return Fetched::Present;
    }
    let (Some(dir), Some(file)) = (config.cache_dir.as_deref(), cache_name(&key)) else {
        return Fetched::NotFound;
    };
    if has_fresh_marker(dir, &file) {
        return Fetched::NotFound;
    }
    let Some(agent) = agent.filter(|_| config.online) else {
        return Fetched::NotFound;
    };
    match fetch_online(agent, name) {
        Ok(Some(bytes)) => match save(dir, &file, &bytes) {
            Some(path) => Fetched::Downloaded(path),
            None => Fetched::Failed,
        },
        Ok(None) => {
            write_notfound(dir, &file);
            Fetched::NotFound
        }
        Err(e) => {
            log::warn!("artist image lookup failed for {name:?}: {e}");
            Fetched::Failed
        }
    }
}

/// Writes image `bytes` to `<dir>/<name>.<ext>` and removes the artist's not-found marker.
fn save(dir: &Path, name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let ext = image_extension(bytes)?;
    let path = dir.join(format!("{name}.{ext}"));
    let part = dir.join(format!("{name}.{ext}.part"));
    let result = fs::create_dir_all(dir)
        .and_then(|()| fs::write(&part, bytes))
        .and_then(|()| fs::rename(&part, &path));
    if let Err(e) = result {
        log::warn!("failed to save {}: {e}", path.display());
        let _ = fs::remove_file(&part);
        return None;
    }
    let _ = fs::remove_file(dir.join(format!("{name}.notfound")));
    Some(path)
}

fn write_notfound(dir: &Path, name: &str) {
    let path = dir.join(format!("{name}.notfound"));
    if let Err(e) = fs::create_dir_all(dir).and_then(|()| fs::write(&path, "theaudiodb deezer\n")) {
        log::warn!("failed to write {}: {e}", path.display());
    }
}

/// The first decodable image from TheAudioDB, then Deezer. `Ok(None)` when both answered and
/// neither has one; an error when a service failed and no image was found.
fn fetch_online(agent: &ureq::Agent, name: &str) -> Result<Option<Vec<u8>>, ureq::Error> {
    let query = utf8_percent_encode(name, NON_ALPHANUMERIC).to_string();
    let mut error = None;

    let audiodb = get_text(agent, &format!("{AUDIODB_SEARCH}{query}"))
        .map(|json| json.map_or_else(Vec::new, |j| audiodb_image_urls(&j, name)));
    let deezer = || {
        get_text(agent, &format!("{DEEZER_SEARCH}{query}"))
            .map(|json| json.and_then(|j| deezer_image_url(&j, name)))
    };
    let try_urls = |urls: Vec<String>, error: &mut Option<ureq::Error>| {
        for url in urls {
            match download(agent, &url) {
                Ok(Some(bytes)) if image_extension(&bytes).is_some() => return Some(bytes),
                Ok(_) => log::debug!("no usable artist image at {url}"),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        None
    };

    match audiodb {
        Ok(urls) => {
            if let Some(bytes) = try_urls(urls, &mut error) {
                return Ok(Some(bytes));
            }
        }
        Err(e) => error = Some(e),
    }
    match deezer() {
        Ok(url) => {
            if let Some(bytes) = try_urls(url.into_iter().collect(), &mut error) {
                return Ok(Some(bytes));
            }
        }
        Err(e) => {
            error.get_or_insert(e);
        }
    }
    match error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// The body of `url`, or `None` on a 404.
fn get_text(agent: &ureq::Agent, url: &str) -> Result<Option<String>, ureq::Error> {
    match agent.get(url).call() {
        Ok(mut resp) => Ok(Some(resp.body_mut().read_to_string()?)),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The bytes at `url` up to [`DOWNLOAD_MAX`], or `None` on a 404.
fn download(agent: &ureq::Agent, url: &str) -> Result<Option<Vec<u8>>, ureq::Error> {
    match agent.get(url).call() {
        Ok(mut resp) => Ok(Some(
            resp.body_mut()
                .with_config()
                .limit(DOWNLOAD_MAX)
                .read_to_vec()?,
        )),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e),
    }
}

enum Status {
    InFlight,
    /// Finished; looked up again after this instant, or never.
    Done(Option<Instant>),
}

/// Looks artist images up on a background thread, one artist at a time, the most recently
/// requested first. Each artist is looked up once per session, except that a failed lookup is
/// retried after a minute and a not-found result after [`NOTFOUND_TTL`]. Used from one thread.
pub struct ArtistFetcher {
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<(String, Fetched)>,
    status: RefCell<HashMap<String, Status>>,
}

impl ArtistFetcher {
    /// `on_ready` is called from the worker thread whenever a lookup finishes.
    pub fn new(config: ArtistConfig, on_ready: Box<dyn Fn() + Send + Sync>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<String>();
        let (res_tx, res_rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("artist-images".into())
            .spawn(move || {
                let agent = config.online.then(crate::lyrics::sources::new_agent);
                let mut pending: Vec<String> = Vec::new();
                loop {
                    if pending.is_empty() {
                        match req_rx.recv() {
                            Ok(artist) => pending.push(artist),
                            Err(_) => return,
                        }
                    }
                    pending.extend(req_rx.try_iter());
                    let Some(artist) = pending.pop() else {
                        continue;
                    };
                    let fetched = lookup(&config, &artist, agent.as_ref());
                    if res_tx.send((first_artist(&artist), fetched)).is_err() {
                        return;
                    }
                    on_ready();
                }
            });
        if let Err(e) = spawned {
            log::warn!("failed to start artist image thread: {e}");
        }
        Self {
            tx: req_tx,
            rx: res_rx,
            status: RefCell::new(HashMap::new()),
        }
    }

    /// Queues a lookup for the first artist of `artist` unless one is in flight or finished
    /// recently.
    pub fn request(&self, artist: &str) {
        let key = first_artist(artist);
        if key.is_empty() {
            return;
        }
        let mut status = self.status.borrow_mut();
        match status.get(&key) {
            Some(Status::InFlight) => return,
            Some(Status::Done(None)) => return,
            Some(Status::Done(Some(at))) if Instant::now() < *at => return,
            _ => {}
        }
        if self.tx.send(artist.to_owned()).is_ok() {
            status.insert(key, Status::InFlight);
        }
    }

    /// Finished lookups since the last call, as artist keys and outcomes.
    pub fn poll(&self) -> Vec<(String, Fetched)> {
        let done: Vec<_> = self.rx.try_iter().collect();
        let mut status = self.status.borrow_mut();
        let now = Instant::now();
        for (key, fetched) in &done {
            let retry = match fetched {
                Fetched::Present | Fetched::Downloaded(_) => None,
                Fetched::NotFound => Some(now + NOTFOUND_TTL),
                Fetched::Failed => Some(now + RETRY_AFTER),
            };
            status.insert(key.clone(), Status::Done(retry));
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::sources::tests::temp_dir;

    const AUDIODB: &str = r#"{"artists":[{"idArtist":"114357","strArtist":"Kendrick Lamar",
        "strArtistThumb":"https://www.theaudiodb.com/images/media/artist/thumb/a.jpg",
        "strArtistWideThumb":"https://r2.theaudiodb.com/images/media/artist/widethumb/w.jpg",
        "strArtistFanart":"https://www.theaudiodb.com/images/media/artist/fanart/f1.jpg",
        "strArtistFanart2":null,
        "strArtistFanart3":"",
        "strArtistFanart4":"https://r2.theaudiodb.com/images/media/artist/fanart/f4.jpg"}]}"#;

    const DEEZER: &str = r#"{"data":[
        {"id":4947074,"name":"Tech N9ne feat. Kendrick Lamar",
         "picture_xl":"https://cdn-images.dzcdn.net/images/artist//1000x1000-000000-80-0-0.jpg"},
        {"id":525046,"name":"Kendrick Lamar",
         "picture_xl":"https://cdn-images.dzcdn.net/images/artist/be0a7c55/1000x1000-000000-80-0-0.jpg"}
        ],"total":2}"#;

    fn config(dir: &Path, online: bool) -> ArtistConfig {
        ArtistConfig {
            manual_dir: dir.join("manual"),
            cache_dir: Some(dir.join("cache")),
            online,
        }
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(2, 2, [10, 20, 30, 255].into())
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn first_artist_splits_and_lowercases() {
        assert_eq!(first_artist("Quality Control, Lil Baby"), "quality control");
        assert_eq!(first_artist("OutKast/Sleepy Brown"), "outkast");
        assert_eq!(first_artist("Mariah Carey & Boyz II Men"), "mariah carey");
        assert_eq!(first_artist("NF; Hopsin"), "nf");
        assert_eq!(first_artist(""), "");
        assert_eq!(first_artist_name(" Kendrick Lamar, SZA"), "Kendrick Lamar");
    }

    #[test]
    fn name_matching() {
        assert!(names_match("kendrick lamar", "Kendrick Lamar"));
        assert!(names_match(" beyonce ", "Beyoncé"));
        assert!(names_match("simon and garfunkel", "Simon & Garfunkel"));
        assert!(names_match("ac/dc", "AC/DC"));
        assert!(names_match("motörhead", "Motorhead"));
        assert!(names_match("!!!", "!!!"));
        assert!(!names_match("!!!", "???"));
        assert!(!names_match(
            "kendrick lamar",
            "Tech N9ne feat. Kendrick Lamar"
        ));
        assert!(!names_match("", ""));
    }

    #[test]
    fn audiodb_prefers_fan_art_of_the_matching_artist() {
        assert_eq!(
            audiodb_image_urls(AUDIODB, "kendrick lamar"),
            [
                "https://www.theaudiodb.com/images/media/artist/fanart/f1.jpg",
                "https://r2.theaudiodb.com/images/media/artist/fanart/f4.jpg",
                "https://r2.theaudiodb.com/images/media/artist/widethumb/w.jpg",
            ]
        );
        assert!(audiodb_image_urls(AUDIODB, "kendrick").is_empty());
        assert!(audiodb_image_urls(r#"{"artists":null}"#, "kendrick lamar").is_empty());
        assert!(audiodb_image_urls("not json", "kendrick lamar").is_empty());
    }

    #[test]
    fn deezer_skips_other_artists_and_placeholders() {
        assert_eq!(
            deezer_image_url(DEEZER, "Kendrick Lamar").as_deref(),
            Some("https://cdn-images.dzcdn.net/images/artist/be0a7c55/1000x1000-000000-80-0-0.jpg")
        );
        assert_eq!(deezer_image_url(r#"{"data":[],"total":0}"#, "x"), None);
        let placeholder = DEEZER.replace("be0a7c55", "");
        assert_eq!(deezer_image_url(&placeholder, "Kendrick Lamar"), None);
        assert!(is_deezer_placeholder(
            "https://e-cdns-images.dzcdn.net/images/artist//1000x1000-000000-80-0-0.jpg"
        ));
        assert!(!is_deezer_placeholder(
            "https://cdn-images.dzcdn.net/images/artist/abc/1000x1000-000000-80-0-0.jpg"
        ));
    }

    #[test]
    fn marker_expiry() {
        let now = SystemTime::now();
        assert!(marker_is_fresh(now - Duration::from_secs(60), now));
        assert!(marker_is_fresh(
            now - NOTFOUND_TTL + Duration::from_secs(1),
            now
        ));
        assert!(!marker_is_fresh(now - NOTFOUND_TTL, now));
        assert!(!marker_is_fresh(now + Duration::from_secs(60), now));
    }

    #[test]
    fn cache_names_are_file_safe() {
        assert_eq!(cache_name("ac\\dc").as_deref(), Some("ac_dc"));
        assert_eq!(cache_name(".hidden").as_deref(), Some("_hidden"));
        assert_eq!(cache_name("  "), None);
        assert!(cache_name(&"é".repeat(200)).unwrap().len() <= MAX_KEY_BYTES);
    }

    #[test]
    fn lookup_order_without_network() {
        let dir = temp_dir("artists");
        let cfg = config(&dir, true);
        let cache = dir.join("cache");

        // A fresh marker ends the lookup before any request.
        write_notfound(&cache, "nobody");
        assert_eq!(lookup(&cfg, "Nobody", None), Fetched::NotFound);
        assert_eq!(lookup(&cfg, "Nobody, Someone", None), Fetched::NotFound);

        // Online lookups off: nothing local means not found, and no marker is written.
        let offline = config(&dir, false);
        assert_eq!(lookup(&offline, "Absent", None), Fetched::NotFound);
        assert!(!cache.join("absent.notfound").exists());
        assert_eq!(lookup(&cfg, "", None), Fetched::NotFound);

        // A saved download is found in the cache and clears the marker.
        let saved = save(&cache, "nobody", &png()).unwrap();
        assert_eq!(saved, cache.join("nobody.png"));
        assert!(!cache.join("nobody.notfound").exists());
        assert_eq!(lookup(&offline, "Nobody", None), Fetched::Present);
        assert_eq!(
            image_paths(&dir.join("manual"), Some(&cache), "nobody"),
            std::slice::from_ref(&saved)
        );

        // The manual folder comes first.
        fs::create_dir_all(dir.join("manual")).unwrap();
        fs::write(dir.join("manual/nobody.jpg"), b"x").unwrap();
        assert_eq!(
            image_paths(&dir.join("manual"), Some(&cache), "Nobody"),
            [dir.join("manual/nobody.jpg"), saved]
        );

        // Undecodable data is never saved.
        assert_eq!(save(&cache, "junk", b"not an image"), None);
        assert!(!cache.join("junk.jpg").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fetcher_reports_each_artist_once() {
        let dir = temp_dir("artist-fetcher");
        save(&dir.join("cache"), "kendrick lamar", &png()).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let fetcher = ArtistFetcher::new(
            config(&dir, false),
            Box::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        fetcher.request("Kendrick Lamar, SZA");
        fetcher.request("kendrick lamar");
        fetcher.request("");
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            fetcher.poll(),
            [("kendrick lamar".to_owned(), Fetched::Present)]
        );
        fetcher.request("Kendrick Lamar");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(fetcher.poll().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore = "requires network"]
    fn online_live() {
        let dir = temp_dir("artists-live");
        let cfg = config(&dir, true);
        let agent = crate::lyrics::sources::new_agent();
        let Fetched::Downloaded(path) = lookup(&cfg, "Kendrick Lamar", Some(&agent)) else {
            panic!("no image for Kendrick Lamar");
        };
        assert!(image::open(&path).is_ok());
        assert_eq!(
            lookup(&cfg, "Kendrick Lamar", Some(&agent)),
            Fetched::Present
        );
        let nonsense = "Zzqxv Nonsense Artist 9381";
        assert_eq!(lookup(&cfg, nonsense, Some(&agent)), Fetched::NotFound);
        assert!(dir
            .join("cache/zzqxv nonsense artist 9381.notfound")
            .exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
