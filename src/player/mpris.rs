//! Desktop media players on the D-Bus session bus, through the MPRIS interface.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::debug;
use percent_encoding::percent_decode_str;
use zbus::blocking::{connection, Connection};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::{Command, Origin, PlayState, PlayerState, SongInfo, IO_TIMEOUT};

const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld mirrors whichever player is active, so following it would duplicate a player.
const PROXY_PLAYERS: [&str; 1] = ["playerctld"];
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
/// Track id the MPRIS specification reserves for "no track".
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// The player properties Encore reads, decoded from D-Bus values.
#[derive(Clone, Debug, PartialEq)]
pub struct Properties {
    pub status: PlayState,
    pub position: Option<Duration>,
    pub shuffle: bool,
    /// `LoopStatus` is `Track` or `Playlist`.
    pub looping: bool,
    pub metadata: Metadata,
}

/// The track metadata Encore reads; strings are empty when the player leaves them out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub track_id: String,
    pub url: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub length: Option<Duration>,
    pub art_url: String,
}

/// A session bus connection and the player it follows.
pub struct Mpris {
    conn: Connection,
    /// Lowercased bus name substring that limits which players are followed.
    pin: Option<String>,
    /// Bus name of the player followed.
    followed: Option<String>,
    /// Track id of the followed player's track, for `SetPosition`.
    track_id: Option<String>,
}

impl Mpris {
    pub fn connect(pin: Option<&str>) -> zbus::Result<Self> {
        let conn = connection::Builder::session()?
            .method_timeout(IO_TIMEOUT)
            .build()?;
        Ok(Self {
            conn,
            pin: pin.map(str::to_lowercase),
            followed: None,
            track_id: None,
        })
    }

    /// The state of the player to follow, picked again on every poll, or `None` when there is
    /// none. Errors are those of the bus connection; a player failing to answer counts as gone.
    pub fn poll(&mut self) -> zbus::Result<Option<PlayerState>> {
        let names: Vec<String> = self
            .conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )?
            .body()
            .deserialize()?;
        let mut names: Vec<String> = names
            .into_iter()
            .filter(|name| is_followable(name, self.pin.as_deref()))
            .collect();
        names.sort_unstable();

        let players: Vec<(String, PlayState)> = if names.len() == 1 {
            names.into_iter().map(|n| (n, PlayState::Stopped)).collect()
        } else {
            names
                .into_iter()
                .map(|n| {
                    let status = self.playback_status(&n).unwrap_or(PlayState::Stopped);
                    (n, status)
                })
                .collect()
        };
        self.followed = choose(&players, self.followed.as_deref()).map(str::to_owned);
        let Some(bus) = self.followed.clone() else {
            self.track_id = None;
            return Ok(None);
        };

        let polled_at = Instant::now();
        let reply = self.conn.call_method(
            Some(bus.as_str()),
            OBJECT_PATH,
            Some(PROPERTIES_IFACE),
            "GetAll",
            &(PLAYER_IFACE,),
        );
        let values: HashMap<String, OwnedValue> = match reply.and_then(|m| m.body().deserialize()) {
            Ok(values) => values,
            Err(e) if is_player_error(&e) => {
                debug!("MPRIS player {bus} did not answer: {e}");
                self.track_id = None;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let props = properties(&values);
        self.track_id = Some(props.metadata.track_id.clone()).filter(|id| is_track_id(id));
        let mut state = player_state(&props, polled_at);
        state.source = Some(label(&bus));
        Ok(Some(state))
    }

    /// Sends `cmd` to the followed player; `state` is its last polled state.
    pub fn apply(&self, cmd: Command, state: &PlayerState) -> zbus::Result<()> {
        let Some(bus) = self.followed.as_deref() else {
            return Ok(());
        };
        let call = |method: &str| {
            self.conn
                .call_method(Some(bus), OBJECT_PATH, Some(PLAYER_IFACE), method, &())
                .map(drop)
        };
        let set = |property: &str, value: Value<'_>| {
            self.conn
                .call_method(
                    Some(bus),
                    OBJECT_PATH,
                    Some(PROPERTIES_IFACE),
                    "Set",
                    &(PLAYER_IFACE, property, value),
                )
                .map(drop)
        };
        match cmd {
            Command::TogglePause => call("PlayPause"),
            Command::Next => call("Next"),
            Command::Previous => call("Previous"),
            Command::SeekTo(pos) => match &self.track_id {
                Some(id) => self
                    .conn
                    .call_method(
                        Some(bus),
                        OBJECT_PATH,
                        Some(PLAYER_IFACE),
                        "SetPosition",
                        &(ObjectPath::try_from(id.as_str())?, micros(pos)),
                    )
                    .map(drop),
                // Without a track id only a relative seek is possible.
                None => self
                    .conn
                    .call_method(
                        Some(bus),
                        OBJECT_PATH,
                        Some(PLAYER_IFACE),
                        "Seek",
                        &(micros(pos) - micros(state.elapsed()),),
                    )
                    .map(drop),
            },
            Command::ToggleRandom => set("Shuffle", Value::from(!state.random)),
            Command::ToggleRepeat => set(
                "LoopStatus",
                Value::from(if state.repeat { "None" } else { "Playlist" }),
            ),
        }
    }

    fn playback_status(&self, bus: &str) -> zbus::Result<PlayState> {
        let value: OwnedValue = self
            .conn
            .call_method(
                Some(bus),
                OBJECT_PATH,
                Some(PROPERTIES_IFACE),
                "Get",
                &(PLAYER_IFACE, "PlaybackStatus"),
            )?
            .body()
            .deserialize()?;
        Ok(play_state(&text(&value).unwrap_or_default()))
    }
}

/// Whether `e` comes from the player rather than the bus connection.
fn is_player_error(e: &zbus::Error) -> bool {
    matches!(
        e,
        zbus::Error::MethodError(..) | zbus::Error::FDO(_) | zbus::Error::Variant(_)
    )
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// Whether the bus name `name` is an MPRIS player to follow: not a proxy for other players, and
/// containing `pin` (lowercase) when given.
pub fn is_followable(name: &str, pin: Option<&str>) -> bool {
    let Some(player) = name.strip_prefix(BUS_PREFIX) else {
        return false;
    };
    let base = player.split('.').next().unwrap_or(player);
    !PROXY_PLAYERS.contains(&base) && pin.is_none_or(|pin| player.to_lowercase().contains(pin))
}

/// The player to follow among `players`, bus names with their playback state: a playing one,
/// staying with `last` while it plays; otherwise `last` while it is still there; otherwise the
/// first.
pub fn choose<'a>(players: &'a [(String, PlayState)], last: Option<&str>) -> Option<&'a str> {
    let find = |name: &str| players.iter().find(|(n, _)| n == name);
    if let Some((name, PlayState::Playing)) = last.and_then(find) {
        return Some(name);
    }
    players
        .iter()
        .find(|(_, s)| *s == PlayState::Playing)
        .or_else(|| last.and_then(find))
        .or(players.first())
        .map(|(name, _)| name.as_str())
}

/// Short name of the player at bus name `bus`, such as `MPRIS: spotify`.
pub fn label(bus: &str) -> String {
    format!("MPRIS: {}", bus.strip_prefix(BUS_PREFIX).unwrap_or(bus))
}

fn play_state(status: &str) -> PlayState {
    match status {
        "Playing" => PlayState::Playing,
        "Paused" => PlayState::Paused,
        _ => PlayState::Stopped,
    }
}

/// Whether `id` names a track that `SetPosition` accepts.
fn is_track_id(id: &str) -> bool {
    id.starts_with('/') && id != NO_TRACK
}

/// The player properties in `values`, the reply of `GetAll`.
pub fn properties(values: &HashMap<String, OwnedValue>) -> Properties {
    let get = |key: &str| values.get(key).map(|v| &**v);
    Properties {
        status: play_state(&get("PlaybackStatus").and_then(text).unwrap_or_default()),
        position: get("Position")
            .and_then(integer)
            .and_then(|us| u64::try_from(us).ok())
            .map(Duration::from_micros),
        shuffle: matches!(get("Shuffle").map(unwrap), Some(Value::Bool(true))),
        looping: get("LoopStatus")
            .and_then(text)
            .is_some_and(|s| s == "Track" || s == "Playlist"),
        metadata: get("Metadata").map(metadata).unwrap_or_default(),
    }
}

/// The track metadata in `value`, an `a{sv}` dictionary.
pub fn metadata(value: &Value<'_>) -> Metadata {
    let Value::Dict(dict) = unwrap(value) else {
        return Metadata::default();
    };
    let entries: HashMap<String, &Value<'_>> = dict
        .iter()
        .filter_map(|(k, v)| Some((text(k)?, v)))
        .collect();
    let text_of = |key: &str| entries.get(key).and_then(|v| text(v)).unwrap_or_default();
    Metadata {
        track_id: text_of("mpris:trackid"),
        url: text_of("xesam:url"),
        title: text_of("xesam:title"),
        artists: entries
            .get("xesam:artist")
            .map(|v| texts(v))
            .unwrap_or_default(),
        album: text_of("xesam:album"),
        length: entries
            .get("mpris:length")
            .and_then(|v| integer(v))
            .and_then(|us| u64::try_from(us).ok())
            .filter(|&us| us > 0)
            .map(Duration::from_micros),
        art_url: text_of("mpris:artUrl"),
    }
}

/// `value` with any variant wrappers removed.
fn unwrap<'a, 'v>(value: &'a Value<'v>) -> &'a Value<'v> {
    match value {
        Value::Value(inner) => unwrap(inner),
        other => other,
    }
}

fn text(value: &Value<'_>) -> Option<String> {
    match unwrap(value) {
        Value::Str(s) => Some(s.as_str().trim().to_owned()),
        Value::ObjectPath(p) => Some(p.as_str().to_owned()),
        _ => None,
    }
}

/// A list of strings; players that send a single string instead of a list are accepted too.
fn texts(value: &Value<'_>) -> Vec<String> {
    match unwrap(value) {
        Value::Array(items) => items
            .inner()
            .iter()
            .filter_map(text)
            .filter(|s| !s.is_empty())
            .collect(),
        other => text(other).into_iter().filter(|s| !s.is_empty()).collect(),
    }
}

/// An integer of any width; players disagree on the type of lengths and positions.
fn integer(value: &Value<'_>) -> Option<i64> {
    match *unwrap(value) {
        Value::I64(n) => Some(n),
        Value::U64(n) => i64::try_from(n).ok(),
        Value::I32(n) => Some(n.into()),
        Value::U32(n) => Some(n.into()),
        Value::I16(n) => Some(n.into()),
        Value::U16(n) => Some(n.into()),
        Value::F64(n) if n.is_finite() => Some(n as i64),
        _ => None,
    }
}

/// The state shown for a player with `props`, read at `polled_at`.
pub fn player_state(props: &Properties, polled_at: Instant) -> PlayerState {
    let current = song_info(&props.metadata);
    let elapsed = match props.status {
        PlayState::Stopped => Duration::ZERO,
        _ => props.position.unwrap_or_default(),
    };
    PlayerState {
        connected: true,
        queue_len: u32::from(current.is_some()),
        current,
        next: None,
        state: props.status,
        random: props.shuffle,
        repeat: props.looping,
        elapsed_at_poll: elapsed,
        polled_at: Some(polled_at),
        ..PlayerState::default()
    }
}

/// The song described by `meta`, or `None` when it describes no track.
pub fn song_info(meta: &Metadata) -> Option<SongInfo> {
    let has_track = is_track_id(&meta.track_id);
    if meta.title.is_empty() && meta.url.is_empty() && !has_track {
        return None;
    }
    let file = song_file(meta);
    let title = if meta.title.is_empty() {
        url_stem(&meta.url).unwrap_or_default()
    } else {
        meta.title.clone()
    };
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    Some(SongInfo {
        id: hasher.finish() as u32,
        title,
        artist: meta.artists.join(", "),
        album: meta.album.clone(),
        duration: meta.length,
        origin: Origin::Mpris {
            art_url: Some(meta.art_url.clone()).filter(|u| !u.is_empty()),
        },
        file,
    })
}

/// A string identifying the track: its `file://` URL for a local file, else the URL or track
/// id with the artist and title added, since some players reuse one URL or id for every track.
fn song_file(meta: &Metadata) -> String {
    if file_url_path(&meta.url).is_some() {
        return meta.url.clone();
    }
    let base = if !meta.url.is_empty() {
        meta.url.clone()
    } else if is_track_id(&meta.track_id) {
        format!("mpris://{}", meta.track_id.trim_start_matches('/'))
    } else {
        "mpris://".to_owned()
    };
    format!("{base}#{} - {}", meta.artists.join(", "), meta.title)
}

/// Name of the file or page a URL points to, without its extension.
fn url_stem(url: &str) -> Option<String> {
    if let Some(path) = file_url_path(url) {
        return path.file_stem().map(|s| s.to_string_lossy().into_owned());
    }
    let last = url
        .split(['?', '#'])
        .next()?
        .trim_end_matches('/')
        .rsplit('/')
        .next()?;
    let decoded = percent_decode_str(last).decode_utf8_lossy();
    Path::new(decoded.as_ref())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// The local path of a `file://` URL, percent-decoded.
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = match rest.strip_prefix("localhost") {
        Some(path) => path,
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(
        percent_decode_str(path).decode_utf8_lossy().into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Array, Dict, Signature};

    fn meta(title: &str, url: &str) -> Metadata {
        Metadata {
            title: title.into(),
            url: url.into(),
            artists: vec!["A".into(), "B".into()],
            album: "Album".into(),
            length: Some(Duration::from_secs(200)),
            ..Metadata::default()
        }
    }

    fn owned(value: Value<'_>) -> OwnedValue {
        value.try_into().unwrap()
    }

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(
            file_url_path("file:///home/u/Music/A%20B/01%20%C3%A9.flac"),
            Some(PathBuf::from("/home/u/Music/A B/01 é.flac"))
        );
        assert_eq!(
            file_url_path("file://localhost/srv/x.mp3"),
            Some(PathBuf::from("/srv/x.mp3"))
        );
        assert_eq!(file_url_path("file://host/x.mp3"), None);
        assert_eq!(file_url_path("https://example.com/x.mp3"), None);
        assert_eq!(file_url_path(""), None);
    }

    #[test]
    fn local_file_song() {
        let song = song_info(&meta("Song", "file:///m/a%20b.flac")).unwrap();
        assert_eq!(song.file, "file:///m/a%20b.flac");
        assert_eq!(song.title, "Song");
        assert_eq!(song.artist, "A, B");
        assert_eq!(song.album, "Album");
        assert_eq!(song.duration, Some(Duration::from_secs(200)));
        assert_eq!(song.origin, Origin::Mpris { art_url: None });
        assert_eq!(song.local_path(None), Some(PathBuf::from("/m/a b.flac")));
        assert_eq!(song.lyrics_key().file, "/m/a b.flac");
    }

    #[test]
    fn title_falls_back_to_url_name() {
        let song = song_info(&meta("", "file:///m/01%20Track.flac")).unwrap();
        assert_eq!(song.title, "01 Track");
        let song = song_info(&meta("", "https://example.com/videos/clip.mp4?t=3")).unwrap();
        assert_eq!(song.title, "clip");
    }

    #[test]
    fn streamed_songs_are_told_apart_by_title() {
        let a = song_info(&meta("One", "https://example.com/watch")).unwrap();
        let b = song_info(&meta("Two", "https://example.com/watch")).unwrap();
        assert_ne!(a.file, b.file);
        assert_ne!(a.id, b.id);
        assert_eq!(
            a,
            song_info(&meta("One", "https://example.com/watch")).unwrap()
        );
        // Sidecar lyrics lookup skips URLs.
        assert!(a.lyrics_key().file.contains("://"));
        assert_eq!(a.local_path(Some(Path::new("/m"))), None);
    }

    #[test]
    fn track_id_identifies_a_song_without_url() {
        let mut m = meta("Song", "");
        m.track_id = "/com/spotify/track/abc".into();
        m.art_url = "https://i.scdn.co/image/x".into();
        let song = song_info(&m).unwrap();
        assert!(song.file.starts_with("mpris://com/spotify/track/abc#"));
        assert_eq!(
            song.origin,
            Origin::Mpris {
                art_url: Some("https://i.scdn.co/image/x".into())
            }
        );
    }

    #[test]
    fn empty_metadata_is_no_song() {
        assert_eq!(song_info(&Metadata::default()), None);
        let no_track = Metadata {
            track_id: NO_TRACK.into(),
            ..Metadata::default()
        };
        assert_eq!(song_info(&no_track), None);
    }

    #[test]
    fn player_state_from_properties() {
        let props = Properties {
            status: PlayState::Paused,
            position: Some(Duration::from_secs(50)),
            shuffle: true,
            looping: false,
            metadata: meta("Song", "https://example.com/a"),
        };
        let state = player_state(&props, Instant::now() - Duration::from_secs(5));
        assert!(state.connected);
        assert_eq!(state.state, PlayState::Paused);
        assert_eq!(state.elapsed(), Duration::from_secs(50));
        assert!((state.progress() - 0.25).abs() < 1e-6);
        assert!(state.random);
        assert!(!state.repeat);
        assert!(state.next.is_none());

        let playing = player_state(
            &Properties {
                status: PlayState::Playing,
                ..props.clone()
            },
            Instant::now() - Duration::from_secs(5),
        );
        assert!(playing.elapsed() >= Duration::from_secs(55));

        let stopped = player_state(
            &Properties {
                status: PlayState::Stopped,
                metadata: Metadata::default(),
                ..props
            },
            Instant::now(),
        );
        assert!(stopped.connected);
        assert!(stopped.current.is_none());
        assert_eq!(stopped.elapsed(), Duration::ZERO);
    }

    #[test]
    fn properties_from_dbus_values() {
        let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
        dict.add(
            "mpris:trackid",
            Value::from(ObjectPath::try_from("/t/1").unwrap()),
        )
        .unwrap();
        dict.add("xesam:title", Value::from(" Song ")).unwrap();
        let mut artists = Array::new(&Signature::Str);
        artists.append(Value::from("A")).unwrap();
        artists.append(Value::from("")).unwrap();
        artists.append(Value::from("B")).unwrap();
        dict.add("xesam:artist", Value::from(artists)).unwrap();
        dict.add("mpris:length", Value::from(215_000_000u64))
            .unwrap();
        dict.add("mpris:artUrl", Value::from("file:///c.jpg"))
            .unwrap();

        let values: HashMap<String, OwnedValue> = [
            ("PlaybackStatus", owned(Value::from("Playing"))),
            ("Position", owned(Value::from(42_000_000i64))),
            ("Shuffle", owned(Value::from(false))),
            ("LoopStatus", owned(Value::from("Track"))),
            ("Metadata", owned(Value::from(dict))),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let props = properties(&values);
        assert_eq!(props.status, PlayState::Playing);
        assert_eq!(props.position, Some(Duration::from_secs(42)));
        assert!(!props.shuffle);
        assert!(props.looping);
        let m = &props.metadata;
        assert_eq!(m.track_id, "/t/1");
        assert_eq!(m.title, "Song");
        assert_eq!(m.artists, ["A", "B"]);
        assert_eq!(m.length, Some(Duration::from_secs(215)));
        assert_eq!(m.art_url, "file:///c.jpg");
        assert_eq!(m.url, "");

        let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
        dict.add("xesam:artist", Value::from("Solo")).unwrap();
        dict.add("mpris:length", Value::from(-1i64)).unwrap();
        let m = metadata(&Value::from(dict));
        assert_eq!(m.artists, ["Solo"]);
        assert_eq!(m.length, None);
        assert_eq!(properties(&HashMap::new()).status, PlayState::Stopped);
    }

    #[test]
    fn choosing_a_player() {
        let players = |states: &[(&str, PlayState)]| -> Vec<(String, PlayState)> {
            states.iter().map(|(n, s)| (n.to_string(), *s)).collect()
        };
        use PlayState::*;
        let p = players(&[("a", Paused), ("b", Playing), ("c", Playing)]);
        assert_eq!(choose(&p, None), Some("b"));
        assert_eq!(choose(&p, Some("c")), Some("c"));
        assert_eq!(choose(&p, Some("a")), Some("b"));
        let p = players(&[("a", Paused), ("b", Stopped)]);
        assert_eq!(choose(&p, Some("b")), Some("b"));
        assert_eq!(choose(&p, Some("gone")), Some("a"));
        assert_eq!(choose(&p, None), Some("a"));
        assert_eq!(choose(&[], Some("a")), None);
    }

    #[test]
    fn followable_players() {
        assert!(is_followable("org.mpris.MediaPlayer2.spotify", None));
        assert!(is_followable(
            "org.mpris.MediaPlayer2.vlc.instance42",
            Some("vlc")
        ));
        assert!(is_followable(
            "org.mpris.MediaPlayer2.Spotify",
            Some("spotify")
        ));
        assert!(!is_followable(
            "org.mpris.MediaPlayer2.firefox.instance_1",
            Some("spotify")
        ));
        assert!(!is_followable("org.mpris.MediaPlayer2.playerctld", None));
        assert!(!is_followable("org.freedesktop.Notifications", None));
        assert_eq!(label("org.mpris.MediaPlayer2.spotify"), "MPRIS: spotify");
    }

    /// Prints the state of the player that would be followed; set `ENCORE_MPRIS_PLAYER` to pick
    /// one.
    #[test]
    #[ignore = "needs a D-Bus session bus and a running MPRIS player"]
    fn poll_a_running_player() {
        let pin = std::env::var("ENCORE_MPRIS_PLAYER").ok();
        let mut mpris = Mpris::connect(pin.as_deref()).unwrap();
        let state = mpris.poll().unwrap().expect("no MPRIS player");
        println!("{:?} {:?}", state.source, state.state);
        println!("elapsed {:?}", state.elapsed());
        println!("{:#?}", state.current);
        assert!(state.connected);
    }
}
