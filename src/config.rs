use std::path::{Path, PathBuf};

use log::warn;

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: u16 = 6600;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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

/// Horizontal alignment of the Spotify layout's lyrics.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LyricsAlign {
    /// Left edge on the title's left edge.
    Left,
    /// Centred between the title's left edge and the play button's right edge.
    Center,
    /// Right edge on the play button's right edge.
    #[default]
    Right,
    /// The right-hand column, its lines left-aligned within it.
    RightColumnLeftText,
}

impl LyricsAlign {
    pub const ALL: [LyricsAlign; 4] = [
        LyricsAlign::Left,
        LyricsAlign::Center,
        LyricsAlign::Right,
        LyricsAlign::RightColumnLeftText,
    ];

    /// The alignment after this one in the cycle Left → Center → Right → right column with
    /// left-aligned lines.
    pub fn next(self) -> Self {
        match self {
            LyricsAlign::Left => LyricsAlign::Center,
            LyricsAlign::Center => LyricsAlign::Right,
            LyricsAlign::Right => LyricsAlign::RightColumnLeftText,
            LyricsAlign::RightColumnLeftText => LyricsAlign::Left,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            LyricsAlign::Left => "Left",
            LyricsAlign::Center => "Center",
            LyricsAlign::Right => "Right",
            LyricsAlign::RightColumnLeftText => "Right, left-aligned",
        }
    }
}

/// How the Spotify layout's synced lyrics move.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum LyricsFlow {
    /// Three lines: the previous one above the active one and the next one below.
    Classic,
    /// The active line at the top with the next three below it, fading out.
    #[default]
    Stacked,
    /// The whole lyrics scrolling past the active line, as in Apple Music: the active line
    /// larger and brightest, the others smaller and dimmer.
    Scrolling,
}

impl LyricsFlow {
    pub const ALL: [LyricsFlow; 3] = [
        LyricsFlow::Classic,
        LyricsFlow::Stacked,
        LyricsFlow::Scrolling,
    ];

    /// The flow after this one in the cycle Classic → Stacked → Scrolling.
    pub fn next(self) -> Self {
        match self {
            LyricsFlow::Classic => LyricsFlow::Stacked,
            LyricsFlow::Stacked => LyricsFlow::Scrolling,
            LyricsFlow::Scrolling => LyricsFlow::Classic,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            LyricsFlow::Classic => "Classic",
            LyricsFlow::Stacked => "Stacked",
            LyricsFlow::Scrolling => "Scrolling",
        }
    }
}

impl Config {
    /// Whether `layout` starts with the artist image as its background:
    /// `ENCORE_ARTIST_BG` when set, else only for the Spotify layout.
    pub fn artist_bg(&self, layout: Layout) -> bool {
        self.artist_bg.unwrap_or(layout == Layout::Spotify)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mpd_host: String,
    pub mpd_port: u16,
    pub mpd_password: Option<String>,
    /// MPD music root: `MPD_MUSIC_DIR`, else `music_directory` from MPD's config file.
    pub music_dir: Option<PathBuf>,
    /// Folder for exported clips: `ENCORE_EXPORT_DIR`, else `<videos>/encore`.
    pub export_dir: PathBuf,
    pub layout: Layout,
    pub lyrics_enabled: bool,
    pub lyrics_online: bool,
    /// Animated backdrop behind the Apple and Vibrant layouts.
    pub motion: bool,
    /// `ENCORE_ARTIST_BG` for every layout; see [`Config::artist_bg`].
    pub artist_bg: Option<bool>,
    /// Text, icons and bars take their colours from the album art.
    pub tint: bool,
    /// Previous and next buttons at the ends of the Spotify layout's progress bar.
    pub skip_buttons: bool,
    pub lyrics_align: LyricsAlign,
    pub lyrics_flow: LyricsFlow,
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
            lookup_with_legacy(&get, key)
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

        let layout = match get("ENCORE_LAYOUT").map(|v| v.to_lowercase()).as_deref() {
            Some("spotify") => Layout::Spotify,
            Some("vibrant") => Layout::Vibrant,
            Some("apple") | None => Layout::Apple,
            Some(other) => {
                warn!("unknown ENCORE_LAYOUT {other:?}, using apple");
                Layout::Apple
            }
        };

        let lyrics_align = match get("ENCORE_LYRICS_ALIGN")
            .map(|v| v.to_lowercase())
            .as_deref()
        {
            Some("left") => LyricsAlign::Left,
            Some("center" | "centre") => LyricsAlign::Center,
            Some("right") | None => LyricsAlign::Right,
            Some("right-left") => LyricsAlign::RightColumnLeftText,
            Some(other) => {
                warn!("unknown ENCORE_LYRICS_ALIGN {other:?}, using right");
                LyricsAlign::Right
            }
        };

        let lyrics_flow = match get("ENCORE_LYRICS_FLOW")
            .map(|v| v.to_lowercase())
            .as_deref()
        {
            Some("classic") => LyricsFlow::Classic,
            Some("stacked") | None => LyricsFlow::Stacked,
            Some("scrolling") => LyricsFlow::Scrolling,
            Some(other) => {
                warn!("unknown ENCORE_LYRICS_FLOW {other:?}, using stacked");
                LyricsFlow::Stacked
            }
        };

        let home = get("HOME").map(PathBuf::from);
        let music_dir = match get("MPD_MUSIC_DIR") {
            Some(dir) => Some(expand_tilde(&dir, home.as_deref())),
            None => mpd_conf_candidates(get("XDG_CONFIG_HOME").map(PathBuf::from), home.as_deref())
                .iter()
                .find_map(|path| parse_music_directory(&read_file(path)?, home.as_deref())),
        };

        let export_dir = match get("ENCORE_EXPORT_DIR") {
            Some(dir) => expand_tilde(&dir, home.as_deref()),
            None => video_dir
                .or_else(|| home.as_ref().map(|h| h.join("Videos")))
                .unwrap_or_else(|| PathBuf::from("."))
                .join("encore"),
        };

        Self {
            mpd_host,
            mpd_port,
            mpd_password,
            music_dir,
            export_dir,
            layout,
            lyrics_enabled: get("ENCORE_LYRICS").is_none_or(|v| !is_off(&v)),
            lyrics_online: get("ENCORE_LYRICS_ONLINE").is_none_or(|v| !is_off(&v)),
            motion: get("ENCORE_MOTION").is_none_or(|v| !is_off(&v)),
            artist_bg: get("ENCORE_ARTIST_BG").map(|v| !is_off(&v)),
            tint: get("ENCORE_TINT").is_none_or(|v| !is_off(&v)),
            skip_buttons: get("ENCORE_SKIP_BUTTONS").is_none_or(|v| !is_off(&v)),
            lyrics_align,
            lyrics_flow,
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

/// Variable prefix of the application's settings.
const ENV_PREFIX: &str = "ENCORE_";
/// Prefix older setups may still use; accepted when the `ENCORE_` name is unset.
const LEGACY_ENV_PREFIX: &str = "MPD_DISPLAY_";

/// Looks up `key`; for an `ENCORE_*` key that is unset, the `MPD_DISPLAY_*` variable of the same
/// name is used instead.
fn lookup_with_legacy(get: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key).or_else(|| {
        let rest = key.strip_prefix(ENV_PREFIX)?;
        get(&format!("{LEGACY_ENV_PREFIX}{rest}"))
    })
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
        assert_eq!(cfg.export_dir, PathBuf::from("./encore"));
        assert_eq!(cfg.layout, Layout::Apple);
        assert!(cfg.lyrics_enabled);
        assert!(cfg.lyrics_online);
        assert!(cfg.motion);
        assert!(cfg.tint);
        assert!(cfg.skip_buttons);
        assert_eq!(cfg.lyrics_align, LyricsAlign::Right);
        assert_eq!(cfg.lyrics_flow, LyricsFlow::Stacked);
    }

    #[test]
    fn lyrics_flow() {
        let flow = |v| parse(&[("ENCORE_LYRICS_FLOW", v)]).lyrics_flow;
        assert_eq!(flow("classic"), LyricsFlow::Classic);
        assert_eq!(flow(" Stacked "), LyricsFlow::Stacked);
        assert_eq!(flow("Scrolling"), LyricsFlow::Scrolling);
        assert_eq!(flow("sideways"), LyricsFlow::Stacked);
        assert_eq!(LyricsFlow::Classic.next(), LyricsFlow::Stacked);
        assert_eq!(LyricsFlow::Stacked.next(), LyricsFlow::Scrolling);
        assert_eq!(LyricsFlow::Scrolling.next(), LyricsFlow::Classic);
    }

    #[test]
    fn lyrics_alignment() {
        let align = |v| parse(&[("ENCORE_LYRICS_ALIGN", v)]).lyrics_align;
        assert_eq!(align("left"), LyricsAlign::Left);
        assert_eq!(align(" Center "), LyricsAlign::Center);
        assert_eq!(align("centre"), LyricsAlign::Center);
        assert_eq!(align("RIGHT"), LyricsAlign::Right);
        assert_eq!(align("Right-Left"), LyricsAlign::RightColumnLeftText);
        assert_eq!(align("diagonal"), LyricsAlign::Right);
        assert_eq!(LyricsAlign::Left.next(), LyricsAlign::Center);
        assert_eq!(LyricsAlign::Center.next(), LyricsAlign::Right);
        assert_eq!(LyricsAlign::Right.next(), LyricsAlign::RightColumnLeftText);
        assert_eq!(LyricsAlign::RightColumnLeftText.next(), LyricsAlign::Left);
    }

    #[test]
    fn tint_and_skip_button_switches() {
        let cfg = parse(&[("ENCORE_TINT", "off"), ("ENCORE_SKIP_BUTTONS", "OFF")]);
        assert!(!cfg.tint);
        assert!(!cfg.skip_buttons);
        let cfg = parse(&[("ENCORE_TINT", "on"), ("ENCORE_SKIP_BUTTONS", "on")]);
        assert!(cfg.tint);
        assert!(cfg.skip_buttons);
        assert!(!parse(&[("ENCORE_TINT", "0")]).tint);
    }

    #[test]
    fn motion_switch() {
        assert!(!parse(&[("ENCORE_MOTION", "off")]).motion);
        assert!(!parse(&[("ENCORE_MOTION", "0")]).motion);
        assert!(parse(&[("ENCORE_MOTION", "on")]).motion);
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
            ("ENCORE_LAYOUT", "Spotify"),
            ("ENCORE_LYRICS", "off"),
            ("ENCORE_LYRICS_ONLINE", "0"),
            ("MPD_MUSIC_DIR", "/srv/music"),
        ]);
        assert_eq!(cfg.layout, Layout::Spotify);
        assert!(!cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
        assert_eq!(cfg.music_dir, Some(PathBuf::from("/srv/music")));

        let cfg = parse(&[
            ("ENCORE_LAYOUT", "bogus"),
            ("ENCORE_LYRICS_ONLINE", "false"),
        ]);
        assert_eq!(cfg.layout, Layout::Apple);
        assert!(cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
    }

    #[test]
    fn artist_background_defaults_and_override() {
        let cfg = parse(&[]);
        assert_eq!(cfg.artist_bg, None);
        assert!(cfg.artist_bg(Layout::Spotify));
        assert!(!cfg.artist_bg(Layout::Apple));
        assert!(!cfg.artist_bg(Layout::Vibrant));

        let on = parse(&[("ENCORE_ARTIST_BG", "on")]);
        assert!([Layout::Spotify, Layout::Apple, Layout::Vibrant]
            .iter()
            .all(|&l| on.artist_bg(l)));
        let off = parse(&[("ENCORE_ARTIST_BG", "OFF")]);
        assert!([Layout::Spotify, Layout::Apple, Layout::Vibrant]
            .iter()
            .all(|&l| !off.artist_bg(l)));
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
            parse(&[("ENCORE_LAYOUT", "vibrant")]).layout,
            Layout::Vibrant
        );
        assert_eq!(
            parse(&[("ENCORE_LAYOUT", " Vibrant ")]).layout,
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
        assert_eq!(cfg.export_dir, PathBuf::from("/home/u/Videos/encore"));

        let cfg = Config::from_sources(env(&[]), read, Some("/vids".into()));
        assert_eq!(cfg.music_dir, Some("/var/lib/mpd/music".into()));
        assert_eq!(cfg.export_dir, PathBuf::from("/vids/encore"));

        let cfg = Config::from_sources(
            env(&[
                ("HOME", "/home/u"),
                ("MPD_MUSIC_DIR", "~/Override"),
                ("ENCORE_EXPORT_DIR", "~/clips"),
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

    #[test]
    fn new_variable_name_wins_over_legacy_name() {
        let cfg = parse(&[
            ("ENCORE_LAYOUT", "spotify"),
            ("MPD_DISPLAY_LAYOUT", "vibrant"),
        ]);
        assert_eq!(cfg.layout, Layout::Spotify);
    }

    #[test]
    fn legacy_variable_name_is_a_fallback() {
        let cfg = parse(&[
            ("MPD_DISPLAY_LAYOUT", "vibrant"),
            ("MPD_DISPLAY_TINT", "off"),
        ]);
        assert_eq!(cfg.layout, Layout::Vibrant);
        assert!(!cfg.tint);
        let cfg = parse(&[("MPD_DISPLAY_EXPORT_DIR", "/legacy")]);
        assert_eq!(cfg.export_dir, PathBuf::from("/legacy"));
    }

    #[test]
    fn legacy_fallback_does_not_apply_to_mpd_variables() {
        let get = |key: &str| (key == "MPD_DISPLAY_HOST").then(|| "x".to_string());
        assert_eq!(lookup_with_legacy(&get, "MPD_HOST"), None);
    }
}
