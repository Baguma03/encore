//! The optional configuration file, `$XDG_CONFIG_HOME/encore/config.toml`. Its settings sit
//! between the built-in defaults and the environment variables: each key stands in for the
//! variable of the same meaning when that variable is unset. The file is only read; settings
//! changed while running are not written back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::{info, warn};
use toml::{Table, Value};

/// Commented-out starter file written when none exists; every key shows its default.
pub const STARTER: &str = r#"# Encore configuration.
#
# Every setting is optional: remove the leading `#` to change one. Settings left commented out
# use the default shown. An ENCORE_* or MPD_* environment variable overrides the matching
# setting here. Encore only reads this file: changes made with the keyboard while it runs are
# not saved.

# Where playback comes from: "auto" (MPD while it is reachable, otherwise a desktop media player
# over MPRIS), "mpd" or "mpris".
# source = "auto"

# Follow only the MPRIS player whose bus name contains this text, such as "spotify" or
# "firefox". Left unset, the playing player is followed.
# mpris_player = "spotify"

# Starting layout: "apple", "vibrant" or "spotify".
# layout = "apple"

# Lyrics on or off.
# lyrics = true

# Look lyrics up on LRCLIB when there is no local file.
# lyrics_online = true

# Spotify layout lyrics alignment: "left", "center", "right" or "right-left" (the right-hand
# column with left-aligned lines).
# lyrics_align = "right"

# How the Spotify layout's synced lyrics move: "classic", "stacked" or "scrolling".
# lyrics_flow = "stacked"

# Moving album art backdrop.
# motion = true

# Artist image as the background in every layout. Left unset, it is on for the Spotify layout
# and off for the others.
# artist_bg = true

# Text, icons and bars take their colours from the album art.
# tint = true

# Previous and next buttons beside the Spotify layout's progress bar.
# skip_buttons = true

# Date and clock overlay shown at startup.
# clock = true

# Folder for exported clips. Defaults to "~/Videos/encore".
# export_dir = "~/Videos/encore"

[mpd]
# MPD server host. For a server with a password, use "password@host".
# host = "localhost"

# MPD server port.
# port = 6600

# MPD music directory, for sidecar lyrics files and audio in exported clips. Left unset, it is
# read from MPD's own config file.
# music_dir = "~/Music"
"#;

/// Path of the file: `$XDG_CONFIG_HOME/encore/config.toml`, else under `~/.config`.
pub fn path(xdg_config: Option<PathBuf>, home: Option<&Path>) -> Option<PathBuf> {
    let base = xdg_config.or_else(|| home.map(|h| h.join(".config")))?;
    Some(base.join("encore").join("config.toml"))
}

/// Writes [`STARTER`] to `path` when nothing is there. Never replaces an existing file; a
/// failure is only logged.
pub fn write_starter_if_missing(path: &Path) {
    if path.exists() {
        return;
    }
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
        })
        .and_then(|mut file| std::io::Write::write_all(&mut file, STARTER.as_bytes()));
    match result {
        Ok(()) => info!("wrote a starter config to {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => warn!(
            "could not write a starter config to {}: {e}",
            path.display()
        ),
    }
}

/// The kind of value a key takes.
enum Kind {
    Bool,
    /// One of these strings, compared case-insensitively.
    Choice(&'static [&'static str]),
    Text,
    Port,
}

/// Keys of the top level and of `[mpd]`, with the variable each stands in for.
const TOP_KEYS: &[(&str, &str, Kind)] = &[
    (
        "source",
        "ENCORE_SOURCE",
        Kind::Choice(&["auto", "mpd", "mpris"]),
    ),
    ("mpris_player", "ENCORE_MPRIS_PLAYER", Kind::Text),
    (
        "layout",
        "ENCORE_LAYOUT",
        Kind::Choice(&["apple", "vibrant", "spotify"]),
    ),
    ("lyrics", "ENCORE_LYRICS", Kind::Bool),
    ("lyrics_online", "ENCORE_LYRICS_ONLINE", Kind::Bool),
    (
        "lyrics_align",
        "ENCORE_LYRICS_ALIGN",
        Kind::Choice(&["left", "center", "centre", "right", "right-left"]),
    ),
    (
        "lyrics_flow",
        "ENCORE_LYRICS_FLOW",
        Kind::Choice(&["classic", "stacked", "scrolling"]),
    ),
    ("motion", "ENCORE_MOTION", Kind::Bool),
    ("artist_bg", "ENCORE_ARTIST_BG", Kind::Bool),
    ("tint", "ENCORE_TINT", Kind::Bool),
    ("skip_buttons", "ENCORE_SKIP_BUTTONS", Kind::Bool),
    ("clock", "ENCORE_CLOCK", Kind::Bool),
    ("export_dir", "ENCORE_EXPORT_DIR", Kind::Text),
];
const MPD_KEYS: &[(&str, &str, Kind)] = &[
    ("host", "MPD_HOST", Kind::Text),
    ("port", "MPD_PORT", Kind::Port),
    ("music_dir", "MPD_MUSIC_DIR", Kind::Text),
];

/// The file's settings keyed by the variable each stands in for, in the variables' own
/// spelling (`on`/`off` for switches). Unknown keys and bad values are logged and left out, so
/// one mistake only drops that setting; a file that is not valid TOML gives no settings.
pub fn parse(text: &str) -> HashMap<&'static str, String> {
    let table: Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            warn!("ignoring the config file, it is not valid TOML: {e}");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    read_table(&table, "", TOP_KEYS, &mut out);
    match table.get("mpd") {
        Some(Value::Table(mpd)) => read_table(mpd, "mpd.", MPD_KEYS, &mut out),
        Some(other) => warn!("ignoring config key \"mpd\": expected a table, got {other}"),
        None => {}
    }
    out
}

fn read_table(
    table: &Table,
    prefix: &str,
    keys: &[(&str, &'static str, Kind)],
    out: &mut HashMap<&'static str, String>,
) {
    for (key, value) in table {
        if prefix.is_empty() && key == "mpd" {
            continue;
        }
        let Some((_, var, kind)) = keys.iter().find(|(k, ..)| k == key) else {
            warn!("ignoring unknown config key \"{prefix}{key}\"");
            continue;
        };
        match convert(value, kind) {
            Some(v) => {
                out.insert(*var, v);
            }
            None => warn!("ignoring config key \"{prefix}{key}\": invalid value {value}"),
        }
    }
}

/// `value` in its variable's spelling, or `None` when it is not a valid `kind`.
fn convert(value: &Value, kind: &Kind) -> Option<String> {
    match (kind, value) {
        (Kind::Bool, Value::Boolean(b)) => Some(if *b { "on" } else { "off" }.to_owned()),
        (Kind::Choice(choices), Value::String(s)) => {
            let s = s.trim().to_lowercase();
            choices.contains(&s.as_str()).then_some(s)
        }
        (Kind::Text, Value::String(s)) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        (Kind::Port, Value::Integer(n)) => u16::try_from(*n)
            .ok()
            .filter(|&p| p > 0)
            .map(|p| p.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_file() {
        let values = parse(
            r#"
            source = "MPRIS"
            mpris_player = "spotify"
            layout = "Spotify"
            lyrics = false
            lyrics_online = false
            lyrics_align = "right-left"
            lyrics_flow = "scrolling"
            motion = false
            artist_bg = true
            tint = false
            skip_buttons = false
            clock = false
            export_dir = "~/clips"

            [mpd]
            host = "secret@media.lan"
            port = 6601
            music_dir = "/srv/music"
            "#,
        );
        let expected: HashMap<&str, String> = [
            ("ENCORE_SOURCE", "mpris"),
            ("ENCORE_MPRIS_PLAYER", "spotify"),
            ("ENCORE_LAYOUT", "spotify"),
            ("ENCORE_LYRICS", "off"),
            ("ENCORE_LYRICS_ONLINE", "off"),
            ("ENCORE_LYRICS_ALIGN", "right-left"),
            ("ENCORE_LYRICS_FLOW", "scrolling"),
            ("ENCORE_MOTION", "off"),
            ("ENCORE_ARTIST_BG", "on"),
            ("ENCORE_TINT", "off"),
            ("ENCORE_SKIP_BUTTONS", "off"),
            ("ENCORE_CLOCK", "off"),
            ("ENCORE_EXPORT_DIR", "~/clips"),
            ("MPD_HOST", "secret@media.lan"),
            ("MPD_PORT", "6601"),
            ("MPD_MUSIC_DIR", "/srv/music"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_owned()))
        .collect();
        assert_eq!(values, expected);
    }

    #[test]
    fn partial_file() {
        let values = parse("layout = \"vibrant\"\n[mpd]\nport = 6602\n");
        assert_eq!(values.len(), 2);
        assert_eq!(values["ENCORE_LAYOUT"], "vibrant");
        assert_eq!(values["MPD_PORT"], "6602");
        assert!(parse("").is_empty());
    }

    #[test]
    fn bad_values_drop_only_their_key() {
        let values = parse(
            r#"
            layout = "sideways"
            source = "pulse"
            mpris_player = 4
            lyrics = "maybe"
            tint = false
            lyrics_flow = 3
            colour = "red"
            export_dir = ""

            [mpd]
            port = 70000
            host = "box"
            "#,
        );
        let mut keys: Vec<_> = values.keys().copied().collect();
        keys.sort_unstable();
        assert_eq!(keys, ["ENCORE_TINT", "MPD_HOST"]);
        assert!(parse("mpd = 3\nmotion = false").contains_key("ENCORE_MOTION"));
        assert_eq!(parse("[mpd]\nport = 0").get("MPD_PORT"), None);
    }

    #[test]
    fn invalid_toml_gives_no_settings() {
        assert!(parse("layout = \"spotify\"\nthis is not toml").is_empty());
    }

    #[test]
    fn starter_sets_nothing() {
        assert!(parse(STARTER).is_empty());
    }

    #[test]
    fn file_path() {
        let home = Path::new("/home/u");
        assert_eq!(
            path(Some("/xdg".into()), Some(home)),
            Some(PathBuf::from("/xdg/encore/config.toml"))
        );
        assert_eq!(
            path(None, Some(home)),
            Some(PathBuf::from("/home/u/.config/encore/config.toml"))
        );
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn starter_is_written_once() {
        let dir = std::env::temp_dir().join(format!("encore-config-test-{}", std::process::id()));
        let file = dir.join("encore").join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        write_starter_if_missing(&file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), STARTER);
        std::fs::write(&file, "layout = \"spotify\"\n").unwrap();
        write_starter_if_missing(&file);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "layout = \"spotify\"\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
