use std::path::{Path, PathBuf};

use log::warn;

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: u16 = 6600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Spotify,
    Apple,
    Vibrant,
}

impl Layout {
    /// The layout after this one in the cycle Apple → Vibrant → Spotify.
    pub fn next(self) -> Self {
        match self {
            Layout::Apple => Layout::Vibrant,
            Layout::Vibrant => Layout::Spotify,
            Layout::Spotify => Layout::Apple,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mpd_host: String,
    pub mpd_port: u16,
    pub mpd_password: Option<String>,
    /// MPD music root: `MPD_MUSIC_DIR`, else `music_directory` from MPD's config file.
    pub music_dir: Option<PathBuf>,
    /// Folder for exported clips: `MPD_DISPLAY_EXPORT_DIR`, else `<videos>/mpd-display`.
    pub export_dir: PathBuf,
    pub layout: Layout,
    pub lyrics_enabled: bool,
    pub lyrics_online: bool,
    /// Animated backdrop behind the Apple and Vibrant layouts.
    pub motion: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_sources(
            |key| match std::env::var(key) {
                Ok(v) => Some(v),
                Err(_) if key == "HOME" => {
                    dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
                }
                Err(_) => None,
            },
            |path| std::fs::read_to_string(path).ok(),
            dirs::video_dir(),
        )
    }

    /// Builds the config from a variable lookup alone, reading no files.
    #[cfg(test)]
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        Self::from_sources(get, |_| None, None)
    }

    /// Builds the config from a variable lookup, a file reader and the user's videos folder,
    /// so parsing is testable without the real environment or filesystem.
    pub fn from_sources(
        get: impl Fn(&str) -> Option<String>,
        read_file: impl Fn(&Path) -> Option<String>,
        video_dir: Option<PathBuf>,
    ) -> Self {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        // MPD_HOST follows mpc's `password@host` convention.
        let (mpd_password, mpd_host) = match get("MPD_HOST") {
            Some(raw) => match raw.rsplit_once('@') {
                Some((pass, host)) => (
                    Some(pass.to_owned()).filter(|p| !p.is_empty()),
                    if host.is_empty() {
                        DEFAULT_HOST.to_owned()
                    } else {
                        host.to_owned()
                    },
                ),
                None => (None, raw),
            },
            None => (None, DEFAULT_HOST.to_owned()),
        };

        let mpd_port = match get("MPD_PORT") {
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                warn!("invalid MPD_PORT {raw:?}, using {DEFAULT_PORT}");
                DEFAULT_PORT
            }),
            None => DEFAULT_PORT,
        };

        let layout = match get("MPD_DISPLAY_LAYOUT")
            .map(|v| v.to_lowercase())
            .as_deref()
        {
            Some("spotify") => Layout::Spotify,
            Some("vibrant") => Layout::Vibrant,
            Some("apple") | None => Layout::Apple,
            Some(other) => {
                warn!("unknown MPD_DISPLAY_LAYOUT {other:?}, using apple");
                Layout::Apple
            }
        };

        let home = get("HOME").map(PathBuf::from);
        let music_dir = match get("MPD_MUSIC_DIR") {
            Some(dir) => Some(expand_tilde(&dir, home.as_deref())),
            None => mpd_conf_candidates(get("XDG_CONFIG_HOME").map(PathBuf::from), home.as_deref())
                .iter()
                .find_map(|path| parse_music_directory(&read_file(path)?, home.as_deref())),
        };

        let export_dir = match get("MPD_DISPLAY_EXPORT_DIR") {
            Some(dir) => expand_tilde(&dir, home.as_deref()),
            None => video_dir
                .or_else(|| home.as_ref().map(|h| h.join("Videos")))
                .unwrap_or_else(|| PathBuf::from("."))
                .join("mpd-display"),
        };

        Self {
            mpd_host,
            mpd_port,
            mpd_password,
            music_dir,
            export_dir,
            layout,
            lyrics_enabled: get("MPD_DISPLAY_LYRICS").is_none_or(|v| !is_off(&v)),
            lyrics_online: get("MPD_DISPLAY_LYRICS_ONLINE").is_none_or(|v| !is_off(&v)),
            motion: get("MPD_DISPLAY_MOTION").is_none_or(|v| !is_off(&v)),
        }
    }

    /// Host argument for mpc, which takes the password in the same `password@host` form.
    pub fn mpc_host(&self) -> String {
        match &self.mpd_password {
            Some(pass) => format!("{pass}@{}", self.mpd_host),
            None => self.mpd_host.clone(),
        }
    }
}

/// MPD config files in the order MPD itself searches them.
fn mpd_conf_candidates(xdg_config: Option<PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config) = xdg_config.or_else(|| home.map(|h| h.join(".config"))) {
        paths.push(config.join("mpd").join("mpd.conf"));
    }
    if let Some(home) = home {
        paths.push(home.join(".mpdconf"));
    }
    paths.push(PathBuf::from("/etc/mpd.conf"));
    paths
}

/// Value of the first `music_directory` setting in an mpd.conf, with `~` expanded.
pub fn parse_music_directory(conf: &str, home: Option<&Path>) -> Option<PathBuf> {
    conf.lines().find_map(|line| {
        let line = line.trim_start();
        let rest = line.strip_prefix("music_directory")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let value = parse_conf_value(rest.trim_start())?;
        (!value.is_empty()).then(|| expand_tilde(&value, home))
    })
}

/// A quoted (with `\` escapes) or bare mpd.conf value, ignoring any trailing comment.
fn parse_conf_value(raw: &str) -> Option<String> {
    let Some(quoted) = raw.strip_prefix('"') else {
        return raw.split(['#', ' ', '\t']).next().map(str::to_owned);
    };
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path, home) {
        ("~", Some(home)) => home.to_path_buf(),
        (p, Some(home)) if p.starts_with("~/") => home.join(&p[2..]),
        (p, _) => PathBuf::from(p),
    }
}

fn is_off(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "0" | "off" | "false" | "no")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn parse(vars: &[(&str, &str)]) -> Config {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn defaults() {
        let cfg = parse(&[]);
        assert_eq!(cfg.mpd_host, "localhost");
        assert_eq!(cfg.mpd_port, 6600);
        assert_eq!(cfg.mpd_password, None);
        assert_eq!(cfg.music_dir, None);
        assert_eq!(cfg.export_dir, PathBuf::from("./mpd-display"));
        assert_eq!(cfg.layout, Layout::Apple);
        assert!(cfg.lyrics_enabled);
        assert!(cfg.lyrics_online);
        assert!(cfg.motion);
    }

    #[test]
    fn motion_switch() {
        assert!(!parse(&[("MPD_DISPLAY_MOTION", "off")]).motion);
        assert!(!parse(&[("MPD_DISPLAY_MOTION", "0")]).motion);
        assert!(parse(&[("MPD_DISPLAY_MOTION", "on")]).motion);
    }

    #[test]
    fn host_with_password() {
        let cfg = parse(&[("MPD_HOST", "secret@media.lan"), ("MPD_PORT", "6601")]);
        assert_eq!(cfg.mpd_host, "media.lan");
        assert_eq!(cfg.mpd_password.as_deref(), Some("secret"));
        assert_eq!(cfg.mpd_port, 6601);
        assert_eq!(cfg.mpc_host(), "secret@media.lan");
    }

    #[test]
    fn plain_host_and_bad_port() {
        let cfg = parse(&[("MPD_HOST", "10.0.0.2"), ("MPD_PORT", "nope")]);
        assert_eq!(cfg.mpd_host, "10.0.0.2");
        assert_eq!(cfg.mpd_password, None);
        assert_eq!(cfg.mpd_port, 6600);
        assert_eq!(cfg.mpc_host(), "10.0.0.2");
    }

    #[test]
    fn layout_and_toggles() {
        let cfg = parse(&[
            ("MPD_DISPLAY_LAYOUT", "Spotify"),
            ("MPD_DISPLAY_LYRICS", "off"),
            ("MPD_DISPLAY_LYRICS_ONLINE", "0"),
            ("MPD_MUSIC_DIR", "/srv/music"),
        ]);
        assert_eq!(cfg.layout, Layout::Spotify);
        assert!(!cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
        assert_eq!(cfg.music_dir, Some(PathBuf::from("/srv/music")));

        let cfg = parse(&[
            ("MPD_DISPLAY_LAYOUT", "bogus"),
            ("MPD_DISPLAY_LYRICS_ONLINE", "false"),
        ]);
        assert_eq!(cfg.layout, Layout::Apple);
        assert!(cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
    }

    #[test]
    fn layout_cycle() {
        assert_eq!(Layout::Apple.next(), Layout::Vibrant);
        assert_eq!(Layout::Vibrant.next(), Layout::Spotify);
        assert_eq!(Layout::Spotify.next(), Layout::Apple);
    }

    #[test]
    fn vibrant_layout_from_env() {
        assert_eq!(
            parse(&[("MPD_DISPLAY_LAYOUT", "vibrant")]).layout,
            Layout::Vibrant
        );
        assert_eq!(
            parse(&[("MPD_DISPLAY_LAYOUT", " Vibrant ")]).layout,
            Layout::Vibrant
        );
    }

    #[test]
    fn music_directory_parsing() {
        let home = Path::new("/home/u");
        let p = |conf: &str| parse_music_directory(conf, Some(home));
        assert_eq!(p(r#"music_directory "~/Music""#), Some(home.join("Music")));
        assert_eq!(
            p("  music_directory\t/srv/music  # main"),
            Some("/srv/music".into())
        );
        assert_eq!(
            p(r#"music_directory "/a b/c\"d" # x"#),
            Some(r#"/a b/c"d"#.into())
        );
        assert_eq!(p("music_directory ~"), Some(home.to_path_buf()));
        assert_eq!(
            p("# music_directory \"/old\"\nplaylist_directory \"/p\"\nmusic_directory \"/new\""),
            Some("/new".into())
        );
        assert_eq!(p(r#"music_directory_x "/nope""#), None);
        assert_eq!(p(r#"music_directory """#), None);
        assert_eq!(p(r#"music_directory "/unterminated"#), None);
        assert_eq!(p(""), None);
        assert_eq!(
            parse_music_directory("music_directory ~/m", None),
            Some("~/m".into())
        );
    }

    #[test]
    fn music_dir_and_export_dir_resolution() {
        let files: HashMap<PathBuf, &str> = [
            (
                PathBuf::from("/home/u/.mpdconf"),
                r#"music_directory "~/Tunes""#,
            ),
            (
                PathBuf::from("/etc/mpd.conf"),
                r#"music_directory "/var/lib/mpd/music""#,
            ),
        ]
        .into_iter()
        .collect();
        let read = |p: &Path| files.get(p).map(|s| s.to_string());
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };

        let cfg = Config::from_sources(env(&[("HOME", "/home/u")]), read, None);
        assert_eq!(cfg.music_dir, Some("/home/u/Tunes".into()));
        assert_eq!(cfg.export_dir, PathBuf::from("/home/u/Videos/mpd-display"));

        let cfg = Config::from_sources(env(&[]), read, Some("/vids".into()));
        assert_eq!(cfg.music_dir, Some("/var/lib/mpd/music".into()));
        assert_eq!(cfg.export_dir, PathBuf::from("/vids/mpd-display"));

        let cfg = Config::from_sources(
            env(&[
                ("HOME", "/home/u"),
                ("MPD_MUSIC_DIR", "~/Override"),
                ("MPD_DISPLAY_EXPORT_DIR", "~/clips"),
            ]),
            read,
            Some("/vids".into()),
        );
        assert_eq!(cfg.music_dir, Some("/home/u/Override".into()));
        assert_eq!(cfg.export_dir, PathBuf::from("/home/u/clips"));

        let xdg = |p: &Path| {
            (p == Path::new("/xdg/mpd/mpd.conf")).then(|| "music_directory /x".to_string())
        };
        let cfg = Config::from_sources(
            env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/xdg")]),
            xdg,
            None,
        );
        assert_eq!(cfg.music_dir, Some("/x".into()));
    }
}
