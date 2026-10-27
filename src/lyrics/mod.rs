//! Lyrics lookup, parsing and caching.

pub mod lrc;
pub mod manual;
pub mod search;
pub mod sources;
pub mod sync_edit;
pub mod timing;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub use manual::read_lyrics_file;
pub use timing::Adjustments;

use timing::{stretch_factor, Timing};

/// Delays before the automatic retries of a requested song whose lookup failed on a network
/// error. While they are pending the song reports `Loading`.
const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(6),
    Duration::from_secs(20),
];
/// Delay before a lookup that failed on a network error may be requested again once the
/// automatic retries are used up.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Lookup threads. With two, a slow prefetch never holds up the current song.
const WORKERS: usize = 2;

/// Identifies a song for lyrics lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SongKey {
    /// MPD-relative file path, an absolute path for a local file played by another player, or
    /// a URL or other identifier for a track with no local file.
    pub file: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration: Option<Duration>,
}

/// A timestamped lyric line. Empty text marks an instrumental gap.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub time: Duration,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lyrics {
    Synced(Vec<LyricLine>),
    Plain(Vec<String>),
}

impl Lyrics {
    /// Index of the line active at `elapsed` (last line with time <= elapsed).
    /// None before the first line or for plain lyrics.
    pub fn current_line(&self, elapsed: Duration) -> Option<usize> {
        match self {
            Lyrics::Synced(lines) => lines.partition_point(|l| l.time <= elapsed).checked_sub(1),
            Lyrics::Plain(_) => None,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Lyrics::Synced(lines) => lines.len(),
            Lyrics::Plain(lines) => lines.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_synced(&self) -> bool {
        matches!(self, Lyrics::Synced(_))
    }
}

/// Lyrics state of a song. `Found` lyrics have the song's timing adjustments applied.
#[derive(Clone, Debug)]
pub enum LyricsState {
    Loading,
    Found(Arc<Lyrics>),
    NotFound,
}

/// An online lyrics service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Lrclib,
    Kugou,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Lrclib => "LRCLIB",
            Provider::Kugou => "KuGou",
        }
    }

    /// Identifier stored in cache files.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Provider::Lrclib => "lrclib",
            Provider::Kugou => "kugou",
        }
    }

    pub(crate) fn from_tag(tag: &str) -> Option<Self> {
        [Provider::Lrclib, Provider::Kugou]
            .into_iter()
            .find(|p| p.tag().eq_ignore_ascii_case(tag.trim()))
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where the shown lyrics came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Saved through the manual lyrics store.
    Manual,
    /// A `.lrc`/`.txt` file next to the audio file.
    Sidecar,
    /// The local lyrics folder.
    Local,
    /// The on-disk cache of an earlier download.
    Cache(Provider),
    /// Downloaded for this lookup.
    Online(Provider),
}

impl fmt::Display for Origin {
    /// "Manual", "Sidecar", "Local", "Cache (LRCLIB)", "LRCLIB", "KuGou".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Manual => f.write_str("Manual"),
            Origin::Sidecar => f.write_str("Sidecar"),
            Origin::Local => f.write_str("Local"),
            Origin::Cache(p) => write!(f, "Cache ({p})"),
            Origin::Online(p) => f.write_str(p.name()),
        }
    }
}

/// Details of a song's found lyrics.
#[derive(Clone, Debug, PartialEq)]
pub struct LyricsInfo {
    pub origin: Origin,
    pub synced: bool,
    /// The text the lyrics were parsed from (LRC or plain), without timing adjustments.
    pub text: Arc<str>,
    pub adjustments: Adjustments,
    /// The `[length:]` tag.
    pub length: Option<Duration>,
    /// Stretching has an effect: the lyrics are synced, carry a `[length:]` tag and the
    /// song's duration is known.
    pub can_stretch: bool,
}

#[derive(Clone, Debug)]
pub struct LyricsConfig {
    /// MPD music root; enables sidecar `.lrc`/`.txt` lookup next to MPD's audio files. Songs
    /// with an absolute path get the lookup without it.
    pub music_dir: Option<PathBuf>,
    /// Folder of `<artist> - <title>.lrc` files.
    pub local_dir: PathBuf,
    /// Cache of downloaded lyrics and not-found markers; also holds manual overrides and
    /// timing adjustments under `manual/`.
    pub cache_dir: Option<PathBuf>,
    /// Enables online lookups.
    pub online: bool,
}

impl LyricsConfig {
    /// `<cache dir>/encore/lyrics`. Cache files from the legacy `mpd-display-rs` folder are
    /// copied over when the new folder does not exist yet.
    pub fn default_cache_dir() -> Option<PathBuf> {
        let base = dirs::cache_dir()?;
        let dir = base.join("encore").join("lyrics");
        migrate_legacy_cache(&base.join("mpd-display-rs").join("lyrics"), &dir);
        Some(dir)
    }
}

/// Copies the files of `old` into `new` when `old` exists and `new` does not. `old` is left intact.
fn migrate_legacy_cache(old: &Path, new: &Path) {
    if new.exists() || !old.is_dir() {
        return;
    }
    let result = std::fs::create_dir_all(new).and_then(|()| {
        for entry in std::fs::read_dir(old)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::copy(entry.path(), new.join(entry.file_name()))?;
            }
        }
        Ok(())
    });
    if let Err(e) = result {
        log::warn!(
            "failed to copy the lyrics cache from {}: {e}",
            old.display()
        );
    }
}

/// Found lyrics before timing adjustments.
struct Raw {
    lyrics: Arc<Lyrics>,
    origin: Origin,
    text: Arc<str>,
}

impl Raw {
    /// The adjusted lyrics and their details.
    fn present(&self, key: &SongKey, adj: Adjustments) -> (LyricsState, Arc<LyricsInfo>) {
        let length = lrc::length_tag(&self.text);
        let timing = Timing::new(adj, length, key.duration);
        let lyrics = if timing.is_identity() {
            Arc::clone(&self.lyrics)
        } else {
            Arc::new(timing.apply(&self.lyrics))
        };
        let synced = self.lyrics.is_synced();
        let info = LyricsInfo {
            origin: self.origin,
            synced,
            text: Arc::clone(&self.text),
            adjustments: adj,
            length,
            can_stretch: synced && stretch_factor(length, key.duration).is_some(),
        };
        (LyricsState::Found(lyrics), Arc::new(info))
    }
}

struct Slot {
    state: LyricsState,
    raw: Option<Raw>,
    info: Option<Arc<LyricsInfo>>,
    /// Set after a transient failure; the key may be looked up again after this instant.
    retry_at: Option<Instant>,
    /// The failure happened in a prefetch, so a request retries at once.
    retry_on_request: bool,
    /// Consecutive transient failures of requested lookups.
    failures: u32,
    /// Lookup results for an older generation are discarded.
    generation: u64,
}

impl Slot {
    fn loading(generation: u64, failures: u32) -> Self {
        Self {
            state: LyricsState::Loading,
            raw: None,
            info: None,
            retry_at: None,
            retry_on_request: false,
            failures,
            generation,
        }
    }
}

#[derive(Default)]
struct Queue {
    slots: HashMap<SongKey, Slot>,
    /// Requested songs, most recent first.
    high: VecDeque<SongKey>,
    low: VecDeque<SongKey>,
    /// Automatic retries of failed requests.
    delayed: Vec<(Instant, SongKey)>,
    next_generation: u64,
    shutdown: bool,
}

impl Queue {
    fn generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    fn unqueue(&mut self, key: &SongKey) {
        self.high.retain(|k| k != key);
        self.low.retain(|k| k != key);
        self.delayed.retain(|(_, k)| k != key);
    }

    /// Next key to look up and whether it was requested; moves due retries into `high`.
    fn pop(&mut self, now: Instant) -> Option<(SongKey, bool)> {
        let (due, waiting) = std::mem::take(&mut self.delayed)
            .into_iter()
            .partition(|(at, _)| *at <= now);
        self.delayed = waiting;
        self.high.extend(due.into_iter().map(|(_, k)| k));
        self.high
            .pop_front()
            .map(|k| (k, true))
            .or_else(|| self.low.pop_front().map(|k| (k, false)))
    }
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    config: LyricsConfig,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Looks up lyrics on background threads and keeps results in memory.
///
/// Requests are served before prefetches, the most recently requested song first. A
/// requested song whose online lookup fails transiently is retried automatically after
/// [`RETRY_BACKOFF`] while it reports `Loading`.
pub struct LyricsProvider {
    shared: Arc<Shared>,
}

impl LyricsProvider {
    /// `on_ready` is called from a worker thread whenever a lookup finishes.
    pub fn new(config: LyricsConfig, on_ready: Box<dyn Fn() + Send + Sync>) -> Self {
        let agent = config.online.then(sources::new_agent);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            config,
        });
        let on_ready: Arc<dyn Fn() + Send + Sync> = Arc::from(on_ready);
        for i in 0..WORKERS {
            let worker = Arc::clone(&shared);
            let on_ready = Arc::clone(&on_ready);
            let agent = agent.clone();
            let spawned = thread::Builder::new()
                .name(format!("lyrics-{i}"))
                .spawn(move || worker_loop(&worker, agent.as_ref(), &*on_ready));
            if let Err(e) = spawned {
                log::warn!("failed to start lyrics worker: {e}");
            }
        }
        Self { shared }
    }

    /// Queues a high-priority lookup unless the song is already known or in flight. The most
    /// recent request is served first.
    pub fn request(&self, key: &SongKey) {
        self.enqueue(key, true);
    }

    /// Queues a low-priority lookup, served after all pending requests.
    pub fn prefetch(&self, key: &SongKey) {
        self.enqueue(key, false);
    }

    /// Current state for a song; unknown songs report `Loading`.
    pub fn get(&self, key: &SongKey) -> LyricsState {
        self.shared
            .lock()
            .slots
            .get(key)
            .map_or(LyricsState::Loading, |s| s.state.clone())
    }

    /// Origin, raw text and adjustments of the song's found lyrics; None while loading or
    /// when no lyrics were found.
    pub fn info(&self, key: &SongKey) -> Option<Arc<LyricsInfo>> {
        self.shared.lock().slots.get(key)?.info.clone()
    }

    /// Forgets the song's lyrics and looks them up again with high priority. A lookup in
    /// flight for the song is discarded.
    pub fn reload(&self, key: &SongKey) {
        let mut q = self.shared.lock();
        q.slots.remove(key);
        q.unqueue(key);
        drop(q);
        self.enqueue(key, true);
    }

    /// Whether the song has manually saved lyrics.
    pub fn has_override(&self, key: &SongKey) -> bool {
        self.shared
            .config
            .cache_dir
            .as_deref()
            .is_some_and(|dir| manual::has_override(dir, key))
    }

    /// Saves `text` (LRC or plain text) as the song's lyrics, ahead of every other source,
    /// and shows it at once with the song's timing adjustments. Fails when there is no cache
    /// directory, the text holds no lyrics, or the file cannot be written.
    pub fn save_override(&self, key: &SongKey, text: &str) -> io::Result<()> {
        let dir = self.cache_dir()?;
        let lyrics = manual::write_override(dir, key, text)?;
        let adj = manual::read_adjustments(dir, key);
        let raw = Raw {
            lyrics: Arc::new(lyrics),
            origin: Origin::Manual,
            text: Arc::from(text),
        };
        let mut q = self.shared.lock();
        q.unqueue(key);
        let generation = q.generation();
        let mut slot = Slot::loading(generation, 0);
        let (state, info) = raw.present(key, adj);
        slot.state = state;
        slot.info = Some(info);
        slot.raw = Some(raw);
        q.slots.insert(key.clone(), slot);
        Ok(())
    }

    /// Removes the song's manual lyrics and looks the song up again.
    pub fn delete_override(&self, key: &SongKey) -> io::Result<()> {
        manual::delete_override(self.cache_dir()?, key)?;
        self.reload(key);
        Ok(())
    }

    /// The song's timing adjustments; defaults when none are saved.
    pub fn adjustments(&self, key: &SongKey) -> Adjustments {
        if let Some(info) = self.info(key) {
            return info.adjustments;
        }
        self.shared
            .config
            .cache_dir
            .as_deref()
            .map(|dir| manual::read_adjustments(dir, key))
            .unwrap_or_default()
    }

    /// Saves the song's timing adjustments and applies them to its shown lyrics at once.
    pub fn set_adjustments(&self, key: &SongKey, adj: Adjustments) -> io::Result<()> {
        manual::write_adjustments(self.cache_dir()?, key, adj)?;
        let mut q = self.shared.lock();
        if let Some(slot) = q.slots.get_mut(key) {
            if let Some(raw) = &slot.raw {
                let (state, info) = raw.present(key, adj);
                slot.state = state;
                slot.info = Some(info);
            }
        }
        Ok(())
    }

    fn cache_dir(&self) -> io::Result<&Path> {
        self.shared
            .config
            .cache_dir
            .as_deref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no lyrics cache directory"))
    }

    fn enqueue(&self, key: &SongKey, high: bool) {
        let mut q = self.shared.lock();
        let mut failures = 0;
        match q.slots.get(key) {
            Some(Slot {
                state: LyricsState::Loading,
                ..
            }) => {
                // Queued or in flight; move a queued key to the front of the requests.
                if high {
                    let queued = match q.low.iter().position(|k| k == key) {
                        Some(i) => q.low.remove(i),
                        None => match q.high.iter().position(|k| k == key) {
                            Some(i) if i > 0 => q.high.remove(i),
                            _ => None,
                        },
                    };
                    if let Some(k) = queued {
                        q.high.push_front(k);
                        self.shared.wake.notify_one();
                    }
                }
                return;
            }
            Some(slot) => {
                let retry = slot
                    .retry_at
                    .is_some_and(|at| Instant::now() >= at || (high && slot.retry_on_request));
                if !retry {
                    return;
                }
                failures = slot.failures;
            }
            None => {}
        }
        let generation = q.generation();
        q.slots
            .insert(key.clone(), Slot::loading(generation, failures));
        if high {
            q.high.push_front(key.clone());
        } else {
            q.low.push_back(key.clone());
        }
        self.shared.wake.notify_one();
    }
}

impl Drop for LyricsProvider {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

fn worker_loop(shared: &Shared, agent: Option<&ureq::Agent>, on_ready: &(dyn Fn() + Send + Sync)) {
    let config = &shared.config;
    loop {
        let (key, high, generation) = {
            let mut q = shared.lock();
            loop {
                if q.shutdown {
                    return;
                }
                let now = Instant::now();
                if let Some((k, high)) = q.pop(now) {
                    match q.slots.get(&k) {
                        Some(s) if matches!(s.state, LyricsState::Loading) => {
                            break (k, high, s.generation);
                        }
                        _ => continue,
                    }
                }
                let next_retry = q.delayed.iter().map(|(at, _)| *at).min();
                q = match next_retry {
                    Some(at) => {
                        let wait = at.saturating_duration_since(now);
                        shared
                            .wake
                            .wait_timeout(q, wait)
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                    }
                    None => shared.wake.wait(q).unwrap_or_else(|e| e.into_inner()),
                };
            }
        };

        let result = sources::lookup(config, &key, agent);
        let adj = match (&result, config.cache_dir.as_deref()) {
            (sources::Lookup::Found(_), Some(dir)) => manual::read_adjustments(dir, &key),
            _ => Adjustments::default(),
        };

        let mut q = shared.lock();
        let Some(slot) = q.slots.get_mut(&key).filter(|s| s.generation == generation) else {
            continue;
        };
        let mut retry = None;
        match result {
            sources::Lookup::Found(found) => {
                let raw = Raw {
                    lyrics: Arc::new(found.lyrics),
                    origin: found.origin,
                    text: Arc::from(found.text),
                };
                let (state, info) = raw.present(&key, adj);
                *slot = Slot::loading(generation, 0);
                slot.state = state;
                slot.info = Some(info);
                slot.raw = Some(raw);
            }
            sources::Lookup::NotFound => {
                *slot = Slot::loading(generation, 0);
                slot.state = LyricsState::NotFound;
            }
            sources::Lookup::Failed => {
                if high {
                    slot.failures += 1;
                }
                let backoff = high
                    .then(|| RETRY_BACKOFF.get(slot.failures as usize - 1))
                    .flatten();
                match backoff {
                    Some(delay) => retry = Some(Instant::now() + *delay),
                    None => {
                        slot.state = LyricsState::NotFound;
                        slot.retry_at = Some(Instant::now() + RETRY_AFTER);
                        slot.retry_on_request = !high;
                    }
                }
            }
        }
        if let Some(at) = retry {
            q.delayed.push((at, key));
            // Another worker may be waiting without a deadline.
            shared.wake.notify_all();
            continue;
        }
        drop(q);
        on_ready();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ms: u64, text: &str) -> LyricLine {
        LyricLine {
            time: Duration::from_millis(ms),
            text: text.into(),
        }
    }

    #[test]
    fn current_line() {
        let l = Lyrics::Synced(vec![line(1000, "a"), line(2000, "b"), line(3000, "c")]);
        let at = |ms| l.current_line(Duration::from_millis(ms));
        assert_eq!(at(0), None);
        assert_eq!(at(999), None);
        assert_eq!(at(1000), Some(0));
        assert_eq!(at(1500), Some(0));
        assert_eq!(at(2000), Some(1));
        assert_eq!(at(10_000), Some(2));
        assert_eq!(l.len(), 3);
        assert!(!l.is_empty());

        let p = Lyrics::Plain(vec!["x".into()]);
        assert_eq!(p.current_line(Duration::from_secs(5)), None);
        assert!(Lyrics::Synced(vec![]).is_empty());
    }

    #[test]
    fn origin_labels() {
        assert_eq!(Origin::Cache(Provider::Kugou).to_string(), "Cache (KuGou)");
        assert_eq!(Origin::Online(Provider::Lrclib).to_string(), "LRCLIB");
        assert_eq!(Origin::Manual.to_string(), "Manual");
        assert_eq!(Provider::from_tag(" KUGOU "), Some(Provider::Kugou));
        assert_eq!(Provider::from_tag("other"), None);
    }

    fn test_provider(dir: &Path) -> (LyricsProvider, std::sync::mpsc::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let provider = LyricsProvider::new(
            LyricsConfig {
                music_dir: None,
                local_dir: dir.join("local"),
                cache_dir: Some(dir.join("cache")),
                online: false,
            },
            Box::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        (provider, rx)
    }

    fn key(title: &str) -> SongKey {
        SongKey {
            file: format!("{title}.flac"),
            artist: "Artist".into(),
            title: title.into(),
            album: String::new(),
            duration: Some(Duration::from_secs(20)),
        }
    }

    #[test]
    fn provider_offline_lookup() {
        let dir = sources::tests::temp_dir("provider");
        let local = dir.join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("Artist - Song.lrc"), "[00:01.00]hi").unwrap();

        let (provider, rx) = test_provider(&dir);
        let (hit, miss) = (key("Song"), key("Other"));
        assert!(matches!(provider.get(&hit), LyricsState::Loading));
        provider.prefetch(&miss);
        provider.request(&hit);
        provider.request(&hit);
        for _ in 0..2 {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(matches!(provider.get(&hit), LyricsState::Found(l) if l.len() == 1));
        assert!(matches!(provider.get(&miss), LyricsState::NotFound));
        let info = provider.info(&hit).unwrap();
        assert_eq!(info.origin, Origin::Local);
        assert!(info.synced);
        assert_eq!(&*info.text, "[00:01.00]hi");
        assert!(provider.info(&miss).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn first_time(provider: &LyricsProvider, key: &SongKey) -> u64 {
        match provider.get(key) {
            LyricsState::Found(l) => match l.as_ref() {
                Lyrics::Synced(v) => v[0].time.as_millis() as u64,
                Lyrics::Plain(_) => panic!("expected synced"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn overrides_and_adjustments() {
        let dir = sources::tests::temp_dir("overrides");
        let local = dir.join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("Artist - Song.lrc"), "[00:01.00]local").unwrap();
        let (provider, rx) = test_provider(&dir);
        let k = key("Song");
        provider.request(&k);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first_time(&provider, &k), 1000);

        // An override is shown at once.
        provider
            .save_override(&k, "[length:00:10.00]\n[00:02.00]manual")
            .unwrap();
        assert!(provider.has_override(&k));
        let info = provider.info(&k).unwrap();
        assert_eq!(info.origin, Origin::Manual);
        assert!(info.can_stretch);
        assert_eq!(first_time(&provider, &k), 2000);

        // Adjustments apply to the shown lyrics at once: 2 s stretched 2x, then 500 ms later.
        let adj = Adjustments {
            offset_ms: 500,
            stretch: true,
        };
        provider.set_adjustments(&k, adj).unwrap();
        assert_eq!(first_time(&provider, &k), 4500);
        assert_eq!(provider.adjustments(&k), adj);

        // Deleting the override falls back to the local file, keeping the adjustments.
        provider.delete_override(&k).unwrap();
        assert!(!provider.has_override(&k));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let info = provider.info(&k).unwrap();
        assert_eq!(info.origin, Origin::Local);
        assert!(!info.can_stretch);
        assert_eq!(first_time(&provider, &k), 1500);

        provider
            .set_adjustments(&k, Adjustments::default())
            .unwrap();
        assert_eq!(first_time(&provider, &k), 1000);
        assert!(provider.save_override(&k, "  ").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn requests_are_served_most_recent_first() {
        let mut q = Queue::default();
        q.high.push_front(key("a"));
        q.high.push_front(key("b"));
        q.low.push_back(key("c"));
        let now = Instant::now();
        q.delayed
            .push((now + Duration::from_secs(60), key("later")));
        q.delayed.push((now, key("due")));
        let order: Vec<(String, bool)> = std::iter::from_fn(|| q.pop(now))
            .map(|(k, high)| (k.title, high))
            .collect();
        let expected = [("b", true), ("a", true), ("due", true), ("c", false)];
        assert_eq!(order, expected.map(|(t, h)| (t.to_owned(), h)).to_vec());
        assert_eq!(q.delayed.len(), 1);
    }

    #[test]
    fn legacy_cache_is_copied_when_new_dir_is_missing() {
        let root = std::env::temp_dir().join(format!("encore-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("old"), root.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.json"), "one").unwrap();

        migrate_legacy_cache(&old, &new);
        assert_eq!(std::fs::read_to_string(new.join("a.json")).unwrap(), "one");
        assert!(old.join("a.json").exists());

        // An existing new folder is never touched.
        std::fs::write(old.join("b.json"), "two").unwrap();
        migrate_legacy_cache(&old, &new);
        assert!(!new.join("b.json").exists());

        // A missing old folder creates nothing.
        let none = root.join("none");
        migrate_legacy_cache(&root.join("absent"), &none);
        assert!(!none.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
