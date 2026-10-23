use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use encore::lyrics::SongKey;
use log::{info, warn};
use mpd::song::Song;
use mpd::status::State;
use mpd::Client;

use crate::config::Config;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub struct SongInfo {
    pub id: u32,
    pub file: String,
    /// Falls back to the file stem when the song has no title tag.
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<Duration>,
}

impl SongInfo {
    fn from_song(song: Song, fallback_id: u32) -> Self {
        let title = song.title.clone().unwrap_or_else(|| {
            Path::new(&song.file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| song.file.clone())
        });
        let album = song
            .tags
            .iter()
            .find(|(k, _)| k == "Album")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        Self {
            id: song.place.map_or(fallback_id, |p| p.id.0),
            title,
            artist: song.artist.unwrap_or_default(),
            album,
            duration: song.duration,
            file: song.file,
        }
    }

    /// Key identifying this song to the lyrics provider.
    pub fn lyrics_key(&self) -> SongKey {
        SongKey {
            file: self.file.clone(),
            artist: self.artist.clone(),
            title: self.title.clone(),
            album: self.album.clone(),
            duration_secs: self.duration.map(|d| d.as_secs()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub connected: bool,
    pub current: Option<SongInfo>,
    pub next: Option<SongInfo>,
    pub state: PlayState,
    pub queue_len: u32,
    pub random: bool,
    pub repeat: bool,
    elapsed_at_poll: Duration,
    /// When `elapsed_at_poll` was read; `None` freezes the elapsed time.
    polled_at: Option<Instant>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            connected: false,
            current: None,
            next: None,
            state: PlayState::Stopped,
            queue_len: 0,
            random: false,
            repeat: false,
            elapsed_at_poll: Duration::ZERO,
            polled_at: Some(Instant::now()),
        }
    }
}

impl PlayerState {
    /// A connected state showing `song` at exactly `elapsed`, which never advances on its own.
    pub fn snapshot(song: SongInfo, elapsed: Duration, state: PlayState) -> Self {
        Self {
            connected: true,
            current: Some(song),
            state,
            elapsed_at_poll: elapsed,
            polled_at: None,
            ..Self::default()
        }
    }

    /// Elapsed time of the current song, interpolated since the last poll while playing.
    pub fn elapsed(&self) -> Duration {
        let elapsed = match self.state {
            PlayState::Playing => {
                self.elapsed_at_poll + self.polled_at.map_or(Duration::ZERO, |t| t.elapsed())
            }
            _ => self.elapsed_at_poll,
        };
        match self.duration() {
            Some(d) => elapsed.min(d),
            None => elapsed,
        }
    }

    /// Fraction of the current song played, in `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        match self.duration() {
            Some(d) if !d.is_zero() => {
                (self.elapsed().as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.current.as_ref().and_then(|s| s.duration)
    }

    /// The fields whose change warrants an immediate repaint.
    fn change_key(&self) -> impl PartialEq + use<'_> {
        (
            self.connected,
            self.current.as_ref().map(|s| s.id),
            self.next.as_ref().map(|s| s.id),
            self.state,
            self.queue_len,
            self.random,
            self.repeat,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    TogglePause,
    Next,
    Previous,
    SeekTo(Duration),
    ToggleRandom,
    ToggleRepeat,
}

/// Polls MPD on a background thread and applies playback commands there.
pub struct Player {
    state: Arc<Mutex<PlayerState>>,
    tx: mpsc::Sender<Command>,
}

impl Player {
    pub fn spawn(cfg: &Config, on_change: Box<dyn Fn() + Send + Sync>) -> Self {
        let state = Arc::new(Mutex::new(PlayerState::default()));
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            addr: format!("{}:{}", cfg.mpd_host, cfg.mpd_port),
            password: cfg.mpd_password.clone(),
            state: Arc::clone(&state),
            on_change,
            client: None,
        };
        let spawned = thread::Builder::new()
            .name("mpd-poll".into())
            .spawn(move || worker.run(rx));
        if let Err(e) = spawned {
            warn!("failed to start MPD thread: {e}");
        }
        Self { state, tx }
    }

    pub fn state(&self) -> PlayerState {
        lock(&self.state).clone()
    }

    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            warn!("MPD thread is gone, dropping {cmd:?}");
        }
    }
}

fn lock(state: &Mutex<PlayerState>) -> MutexGuard<'_, PlayerState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

struct Worker {
    addr: String,
    password: Option<String>,
    state: Arc<Mutex<PlayerState>>,
    on_change: Box<dyn Fn() + Send + Sync>,
    client: Option<Client>,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        loop {
            if self.client.is_none() {
                match self.connect() {
                    Ok(client) => {
                        info!("connected to MPD at {}", self.addr);
                        self.client = Some(client);
                    }
                    Err(e) => {
                        warn!("MPD connection to {} failed: {e}", self.addr);
                        self.publish(PlayerState::default());
                        // Commands issued while disconnected are dropped.
                        match rx.recv_timeout(RECONNECT_INTERVAL) {
                            Err(RecvTimeoutError::Disconnected) => return,
                            _ => continue,
                        }
                    }
                }
            }

            self.poll();

            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(cmd) => {
                    self.apply(cmd);
                    while let Ok(cmd) = rx.try_recv() {
                        self.apply(cmd);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn connect(&self) -> Result<Client, mpd::error::Error> {
        // Try every resolved address: `localhost` often resolves to ::1 first while MPD listens on IPv4 only.
        let mut last_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no address resolved");
        let mut stream = None;
        for addr in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, RECONNECT_INTERVAL) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        let stream = stream.ok_or(last_err)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut client = Client::new(stream)?;
        if let Some(pass) = &self.password {
            client.login(pass)?;
        }
        Ok(client)
    }

    fn poll(&mut self) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match fetch(client) {
            Ok(state) => self.publish(state),
            Err(e) => self.disconnect(&e),
        }
    }

    fn apply(&mut self, cmd: Command) {
        let (random, repeat) = {
            let s = lock(&self.state);
            (s.random, s.repeat)
        };
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let result = match cmd {
            Command::TogglePause => client.toggle_pause(),
            Command::Next => client.next(),
            Command::Previous => client.prev(),
            Command::SeekTo(pos) => client.rewind(pos),
            Command::ToggleRandom => client.random(!random),
            Command::ToggleRepeat => client.repeat(!repeat),
        };
        match result {
            Ok(()) => self.poll(),
            Err(e) => self.disconnect(&e),
        }
        // A seek changes elapsed time without touching the change key.
        (self.on_change)();
    }

    fn disconnect(&mut self, err: &mpd::error::Error) {
        warn!("MPD error, reconnecting: {err}");
        self.client = None;
        self.publish(PlayerState::default());
    }

    fn publish(&self, new: PlayerState) {
        let changed = {
            let mut cur = lock(&self.state);
            let changed = cur.change_key() != new.change_key();
            *cur = new;
            changed
        };
        if changed {
            (self.on_change)();
        }
    }
}

fn fetch(client: &mut Client) -> Result<PlayerState, mpd::error::Error> {
    let status = client.status()?;
    let polled_at = Instant::now();

    let mut current = match status.song {
        Some(place) => client
            .playlistid(place.id)?
            .map(|s| SongInfo::from_song(s, place.id.0)),
        None => None,
    };
    // The status duration has sub-second precision; the song tag only whole seconds.
    if let Some(song) = current.as_mut() {
        song.duration = status
            .duration
            .or(status.time.map(|t| t.1))
            .or(song.duration);
    }
    let next = match status.nextsong {
        Some(place) => client
            .playlistid(place.id)?
            .map(|s| SongInfo::from_song(s, place.id.0)),
        None => None,
    };

    Ok(PlayerState {
        connected: true,
        current,
        next,
        state: match status.state {
            State::Play => PlayState::Playing,
            State::Pause => PlayState::Paused,
            State::Stop => PlayState::Stopped,
        },
        queue_len: status.queue_len,
        random: status.random,
        repeat: status.repeat,
        elapsed_at_poll: status
            .elapsed
            .or(status.time.map(|t| t.0))
            .unwrap_or_default(),
        polled_at: Some(polled_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(duration: u64) -> SongInfo {
        SongInfo {
            id: 1,
            file: "a/b.flac".into(),
            title: "b".into(),
            artist: String::new(),
            album: String::new(),
            duration: Some(Duration::from_secs(duration)),
        }
    }

    #[test]
    fn lyrics_key_copies_tags() {
        let mut info = song(215);
        info.artist = "Artist".into();
        info.album = "Album".into();
        let key = info.lyrics_key();
        assert_eq!(key.file, "a/b.flac");
        assert_eq!(key.title, "b");
        assert_eq!(key.artist, "Artist");
        assert_eq!(key.album, "Album");
        assert_eq!(key.duration_secs, Some(215));
        info.duration = None;
        assert_eq!(info.lyrics_key().duration_secs, None);
    }

    #[test]
    fn paused_progress_is_static() {
        let state = PlayerState {
            connected: true,
            current: Some(song(100)),
            state: PlayState::Paused,
            elapsed_at_poll: Duration::from_secs(25),
            polled_at: Some(Instant::now() - Duration::from_secs(10)),
            ..PlayerState::default()
        };
        assert_eq!(state.elapsed(), Duration::from_secs(25));
        assert!((state.progress() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn playing_interpolates_and_clamps() {
        let mut state = PlayerState {
            connected: true,
            current: Some(song(100)),
            state: PlayState::Playing,
            elapsed_at_poll: Duration::from_secs(25),
            polled_at: Some(Instant::now() - Duration::from_secs(10)),
            ..PlayerState::default()
        };
        assert!(state.elapsed() >= Duration::from_secs(35));
        state.elapsed_at_poll = Duration::from_secs(99);
        assert_eq!(state.elapsed(), Duration::from_secs(100));
        assert_eq!(state.progress(), 1.0);
    }

    #[test]
    fn snapshot_is_frozen() {
        let state =
            PlayerState::snapshot(song(100), Duration::from_millis(42_500), PlayState::Playing);
        assert!(state.connected);
        assert!(state.next.is_none());
        assert_eq!(state.queue_len, 0);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(state.elapsed(), Duration::from_millis(42_500));
        assert!((state.progress() - 0.425).abs() < 1e-6);
        let past_end =
            PlayerState::snapshot(song(100), Duration::from_secs(120), PlayState::Paused);
        assert_eq!(past_end.elapsed(), Duration::from_secs(100));
    }

    #[test]
    fn no_song_has_zero_progress() {
        assert_eq!(PlayerState::default().progress(), 0.0);
    }

    #[test]
    fn title_falls_back_to_file_stem() {
        let song = Song {
            file: "Artist/Album/01 Track.flac".into(),
            ..Song::default()
        };
        let info = SongInfo::from_song(song, 7);
        assert_eq!(info.title, "01 Track");
        assert_eq!(info.id, 7);
    }
}
