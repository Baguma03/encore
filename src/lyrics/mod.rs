//! Lyrics lookup, parsing and caching.

pub mod lrc;
pub mod sources;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Delay before a lookup that failed on a network error may be retried.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Identifies a song for lyrics lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SongKey {
    /// MPD-relative file path.
    pub file: String,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_secs: Option<u64>,
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
}

#[derive(Clone, Debug)]
pub enum LyricsState {
    Loading,
    Found(Arc<Lyrics>),
    NotFound,
}

#[derive(Clone, Debug)]
pub struct LyricsConfig {
    /// MPD music root; enables sidecar `.lrc`/`.txt` lookup next to the audio file.
    pub music_dir: Option<PathBuf>,
    /// Folder of `<artist> - <title>.lrc` files.
    pub local_dir: PathBuf,
    /// Cache of downloaded lyrics and not-found markers.
    pub cache_dir: Option<PathBuf>,
    /// Enables LRCLIB lookups.
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

struct Slot {
    state: LyricsState,
    /// Set after a transient failure; the key may be looked up again after this instant.
    retry_at: Option<Instant>,
}

#[derive(Default)]
struct Queue {
    slots: HashMap<SongKey, Slot>,
    high: VecDeque<SongKey>,
    low: VecDeque<SongKey>,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Looks up lyrics on a single background thread and keeps results in memory.
pub struct LyricsProvider {
    shared: Arc<Shared>,
}

impl LyricsProvider {
    /// `on_ready` is called from the worker thread whenever a lookup finishes.
    pub fn new(config: LyricsConfig, on_ready: Box<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("lyrics".into())
            .spawn(move || worker_loop(worker, config, on_ready));
        if let Err(e) = spawned {
            log::warn!("failed to start lyrics worker: {e}");
        }
        Self { shared }
    }

    /// Queues a high-priority lookup unless the song is already known or in flight.
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

    fn enqueue(&self, key: &SongKey, high: bool) {
        let mut q = self.shared.lock();
        match q.slots.get(key) {
            Some(Slot {
                state: LyricsState::Loading,
                ..
            }) => {
                // Queued or in flight; promote a queued prefetch.
                if high {
                    let before = q.low.len();
                    q.low.retain(|k| k != key);
                    if q.low.len() != before {
                        q.high.push_back(key.clone());
                        self.shared.wake.notify_one();
                    }
                }
                return;
            }
            Some(Slot {
                retry_at: Some(at), ..
            }) if Instant::now() >= *at => {}
            Some(_) => return,
            None => {}
        }
        q.slots.insert(
            key.clone(),
            Slot {
                state: LyricsState::Loading,
                retry_at: None,
            },
        );
        if high {
            q.high.push_back(key.clone());
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

fn worker_loop(shared: Arc<Shared>, config: LyricsConfig, on_ready: Box<dyn Fn() + Send + Sync>) {
    let agent = config.online.then(sources::new_agent);
    loop {
        let key = {
            let mut q = shared.lock();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(k) = q.high.pop_front().or_else(|| q.low.pop_front()) {
                    break k;
                }
                q = shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };

        let slot = match sources::lookup(&config, &key, agent.as_ref()) {
            sources::Lookup::Found(l) => Slot {
                state: LyricsState::Found(Arc::new(l)),
                retry_at: None,
            },
            sources::Lookup::NotFound => Slot {
                state: LyricsState::NotFound,
                retry_at: None,
            },
            sources::Lookup::Failed => Slot {
                state: LyricsState::NotFound,
                retry_at: Some(Instant::now() + RETRY_AFTER),
            },
        };
        shared.lock().slots.insert(key, slot);
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
    fn provider_offline_lookup() {
        let dir = sources::tests::temp_dir("provider");
        let local = dir.join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("Artist - Song.lrc"), "[00:01.00]hi").unwrap();

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let provider = LyricsProvider::new(
            LyricsConfig {
                music_dir: None,
                local_dir: local,
                cache_dir: Some(dir.join("cache")),
                online: false,
            },
            Box::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        let key = |title: &str| SongKey {
            file: format!("{title}.flac"),
            artist: "Artist".into(),
            title: title.into(),
            album: String::new(),
            duration_secs: None,
        };
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
        let _ = std::fs::remove_dir_all(dir);
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
