mod file;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::warn;

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: u16 = 6600;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Layout {
    EncoreClassic,
    Classic,
    Vibrant,
}

impl Layout {
    /// The layout after this one in the cycle Encore Classic → Classic → Vibrant.
    pub fn next(self) -> Self {
        match self {
            Layout::EncoreClassic => Layout::Classic,
            Layout::Classic => Layout::Vibrant,
            Layout::Vibrant => Layout::EncoreClassic,
        }
    }

    /// The layout before this one in the cycle, the reverse of [`Layout::next`].
    pub fn prev(self) -> Self {
        match self {
            Layout::EncoreClassic => Layout::Vibrant,
            Layout::Classic => Layout::EncoreClassic,
            Layout::Vibrant => Layout::Classic,
        }
    }
}

/// Horizontal alignment of the Encore Classic layout's lyrics.
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

/// How the Encore Classic layout's synced lyrics move.
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

/// Where playback state comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Source {
    /// MPD while it is reachable, otherwise an MPRIS player.
    #[default]
    Auto,
    Mpd,
    Mpris,
}

impl Config {
    /// Whether `layout` starts with the artist image as its background:
    /// `ENCORE_ARTIST_BG` or `artist_bg` when set, else only for the Encore Classic layout.
    pub fn artist_bg(&self, layout: Layout) -> bool {
        self.artist_bg.unwrap_or(layout == Layout::EncoreClassic)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mpd_host: String,
    pub mpd_port: u16,
    pub mpd_password: Option<String>,
    pub source: Source,
    /// Case-insensitive substring of the MPRIS bus name to follow, such as `spotify`; any
    /// player when unset.
    pub mpris_player: Option<String>,
    /// MPD music root: `MPD_MUSIC_DIR` or `[mpd] music_dir`, else `music_directory` from MPD's
    /// config file.
    pub music_dir: Option<PathBuf>,
    /// Folder for exported clips: `ENCORE_EXPORT_DIR` or `export_dir`, else `<videos>/encore`.
    pub export_dir: PathBuf,
    pub layout: Layout,
    pub lyrics_enabled: bool,
    pub lyrics_online: bool,
    /// Look up artist images missing locally on TheAudioDB and Deezer.
    pub artists_online: bool,
    /// Animated backdrop behind the Classic and Vibrant layouts.
    pub motion: bool,
    /// `ENCORE_ARTIST_BG` or `artist_bg` for every layout; see [`Config::artist_bg`].
    pub artist_bg: Option<bool>,
    /// Text, icons and bars take their colours from the album art.
    pub tint: bool,
    /// Previous and next buttons at the ends of the Encore Classic layout's progress bar.
    pub skip_buttons: bool,
    pub lyrics_align: LyricsAlign,
    pub lyrics_flow: LyricsFlow,
    /// Date and clock overlay shown at startup.
    pub clock: bool,
    /// Display font at startup: `ENCORE_DISPLAY_FONT` or `display_font`, else the first of
    /// `display_fonts`, else Figtree. A family name or a font file path.
    pub display_font: String,
    /// Display fonts `u` cycles through: `ENCORE_DISPLAY_FONTS` or `display_fonts`, always
    /// holding `display_font`; Figtree and `display_font` when unset.
    pub display_fonts: Vec<String>,
}

impl Config {
    /// Reads the environment variables and the config file, the variables taking precedence.
    pub fn from_env() -> Self {
        let file_values = config_file_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| file::parse(&text))
            .unwrap_or_default();
        Self::from_sources(
            layered(env_var, &file_values),
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

        let source = match get("ENCORE_SOURCE").map(|v| v.to_lowercase()).as_deref() {
            Some("auto") | None => Source::Auto,
            Some("mpd") => Source::Mpd,
            Some("mpris") => Source::Mpris,
            Some(other) => {
                warn!("unknown ENCORE_SOURCE {other:?}, using auto");
                Source::Auto
            }
        };

        let layout = match get("ENCORE_LAYOUT").map(|v| v.to_lowercase()).as_deref() {
            Some("encore-classic") | None => Layout::EncoreClassic,
            Some("classic") => Layout::Classic,
            Some("vibrant") => Layout::Vibrant,
            Some(other) => {
                warn!("unknown ENCORE_LAYOUT {other:?}, using encore-classic");
                Layout::EncoreClassic
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

        let (display_font, display_fonts) = display_fonts(
            get("ENCORE_DISPLAY_FONT"),
            get("ENCORE_DISPLAY_FONTS").map(|v| split_list(&v)),
        );

        Self {
            mpd_host,
            mpd_port,
            mpd_password,
            source,
            mpris_player: get("ENCORE_MPRIS_PLAYER"),
            music_dir,
            export_dir,
            layout,
            lyrics_enabled: get("ENCORE_LYRICS").is_none_or(|v| !is_off(&v)),
            lyrics_online: get("ENCORE_LYRICS_ONLINE").is_none_or(|v| !is_off(&v)),
            artists_online: get("ENCORE_ARTISTS_ONLINE").is_none_or(|v| !is_off(&v)),
            motion: get("ENCORE_MOTION").is_none_or(|v| !is_off(&v)),
            artist_bg: get("ENCORE_ARTIST_BG").map(|v| !is_off(&v)),
            tint: get("ENCORE_TINT").is_none_or(|v| !is_off(&v)),
            skip_buttons: get("ENCORE_SKIP_BUTTONS").is_none_or(|v| !is_off(&v)),
            lyrics_align,
            lyrics_flow,
            clock: get("ENCORE_CLOCK").is_none_or(|v| !is_off(&v)),
            display_font,
            display_fonts,
        }
    }

    /// Position of the startup display font in [`Config::display_fonts`].
    pub fn display_font_index(&self) -> usize {
        self.display_fonts
            .iter()
            .position(|f| same_font(f, &self.display_font))
            .unwrap_or(0)
    }

    /// Host argument for mpc, which takes the password in the same `password@host` form.
    pub fn mpc_host(&self) -> String {
        match &self.mpd_password {
            Some(pass) => format!("{pass}@{}", self.mpd_host),
            None => self.mpd_host.clone(),
        }
    }
}

/// An environment variable, with `HOME` falling back to the platform's home directory.
fn env_var(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) => Some(v),
        Err(_) if key == "HOME" => dirs::home_dir().map(|h| h.to_string_lossy().into_owned()),
        Err(_) => None,
    }
}

/// Path of the config file, from `XDG_CONFIG_HOME` or the home directory.
fn config_file_path() -> Option<PathBuf> {
    let xdg = env_var("XDG_CONFIG_HOME")
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from);
    file::path(xdg, env_var("HOME").map(PathBuf::from).as_deref())
}

/// Writes a commented starter config file when there is none.
pub fn write_starter_config() {
    if let Some(path) = config_file_path() {
        file::write_starter_if_missing(&path);
    }
}

/// A lookup of the variables in `env` (including their legacy names) that falls back to the
/// config file's settings, so a variable always overrides the file.
fn layered<'a>(
    env: impl Fn(&str) -> Option<String> + 'a,
    file_values: &'a HashMap<&'static str, String>,
) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        lookup_with_legacy(&env, key)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| file_values.get(key).cloned())
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

/// Bundled display font, the default.
const DEFAULT_DISPLAY_FONT: &str = "Figtree";

/// Whether two display font entries name the same font.
fn same_font(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The startup display font and the list `u` cycles through, from the `display_font` and
/// `display_fonts` settings. The startup font defaults to the list's first entry, else Figtree;
/// the list defaults to Figtree and the startup font, and always holds the startup font, put
/// first when a configured list lacks it. Repeated entries are dropped.
fn display_fonts(font: Option<String>, list: Option<Vec<String>>) -> (String, Vec<String>) {
    let list = list.filter(|l| !l.is_empty());
    let font = font
        .or_else(|| list.as_ref().and_then(|l| l.first().cloned()))
        .unwrap_or_else(|| DEFAULT_DISPLAY_FONT.to_owned());
    let fonts = match list {
        Some(mut fonts) => {
            if !fonts.iter().any(|f| same_font(f, &font)) {
                fonts.insert(0, font.clone());
            }
            fonts
        }
        None => vec![DEFAULT_DISPLAY_FONT.to_owned(), font.clone()],
    };
    let mut unique: Vec<String> = Vec::with_capacity(fonts.len());
    for f in fonts {
        if !unique.iter().any(|u| same_font(u, &f)) {
            unique.push(f);
        }
    }
    (font, unique)
}

/// The entries of a list setting: one per line when the value has several lines, as the
/// config file passes its lists, otherwise comma-separated, as typed in a variable. Entries
/// are trimmed and empty ones dropped.
pub fn split_list(value: &str) -> Vec<String> {
    let sep = if value.contains('\n') { '\n' } else { ',' };
    value
        .split(sep)
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .collect()
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
        assert_eq!(cfg.source, Source::Auto);
        assert_eq!(cfg.mpris_player, None);
        assert_eq!(cfg.music_dir, None);
        assert_eq!(cfg.export_dir, PathBuf::from("./encore"));
        assert_eq!(cfg.layout, Layout::EncoreClassic);
        assert!(cfg.lyrics_enabled);
        assert!(cfg.lyrics_online);
        assert!(cfg.artists_online);
        assert!(cfg.motion);
        assert!(cfg.tint);
        assert!(cfg.skip_buttons);
        assert_eq!(cfg.lyrics_align, LyricsAlign::Right);
        assert_eq!(cfg.lyrics_flow, LyricsFlow::Stacked);
        assert!(cfg.clock);
    }

    #[test]
    fn display_font_settings() {
        let fonts = |vars: &[(&str, &str)]| {
            let cfg = parse(vars);
            let index = cfg.display_font_index();
            (cfg.display_font, cfg.display_fonts, index)
        };
        let list = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(fonts(&[]), ("Figtree".into(), list(&["Figtree"]), 0));
        assert_eq!(
            fonts(&[("ENCORE_DISPLAY_FONT", " Inter ")]),
            ("Inter".into(), list(&["Figtree", "Inter"]), 1)
        );
        assert_eq!(
            fonts(&[("ENCORE_DISPLAY_FONT", "figtree")]),
            ("figtree".into(), list(&["Figtree"]), 0)
        );
        // A comma-separated variable; the startup font defaults to its first entry.
        assert_eq!(
            fonts(&[(
                "ENCORE_DISPLAY_FONTS",
                "Inter, Poppins,,inter, ~/f/Lato-Regular.ttf"
            )]),
            (
                "Inter".into(),
                list(&["Inter", "Poppins", "~/f/Lato-Regular.ttf"]),
                0
            )
        );
        // The startup font joins the front of a list that lacks it.
        assert_eq!(
            fonts(&[
                ("ENCORE_DISPLAY_FONT", "Lato"),
                ("ENCORE_DISPLAY_FONTS", "Inter,Poppins")
            ]),
            ("Lato".into(), list(&["Lato", "Inter", "Poppins"]), 0)
        );
        assert_eq!(
            fonts(&[
                ("ENCORE_DISPLAY_FONT", "POPPINS"),
                ("MPD_DISPLAY_DISPLAY_FONTS", "Inter,Poppins")
            ]),
            ("POPPINS".into(), list(&["Inter", "Poppins"]), 1)
        );
        // An empty list counts as unset.
        assert_eq!(
            fonts(&[("ENCORE_DISPLAY_FONTS", " , ")]),
            ("Figtree".into(), list(&["Figtree"]), 0)
        );
    }

    #[test]
    fn display_fonts_from_the_config_file() {
        let file_values = file::parse(
            "display_font = \"Inter\"\ndisplay_fonts = [\"Figtree\", \"Inter\", \"/f/A,B.ttf\"]\n",
        );
        let cfg = Config::from_sources(layered(|_| None, &file_values), |_| None, None);
        assert_eq!(cfg.display_font, "Inter");
        assert_eq!(cfg.display_fonts, ["Figtree", "Inter", "/f/A,B.ttf"]);
        assert_eq!(cfg.display_font_index(), 1);
        // A variable replaces the file's list.
        let env = |key: &str| (key == "ENCORE_DISPLAY_FONTS").then(|| "Poppins,Inter".to_owned());
        let cfg = Config::from_sources(layered(env, &file_values), |_| None, None);
        assert_eq!(cfg.display_fonts, ["Poppins", "Inter"]);
        assert_eq!(cfg.display_font_index(), 1);
    }

    #[test]
    fn list_splitting() {
        assert_eq!(split_list("a, b ,,c"), ["a", "b", "c"]);
        assert_eq!(split_list("a,b\nc"), ["a,b", "c"]);
        assert!(split_list(" ").is_empty());
    }

    #[test]
    fn clock_switch() {
        assert!(!parse(&[("ENCORE_CLOCK", "off")]).clock);
        assert!(parse(&[("ENCORE_CLOCK", "on")]).clock);
    }

    #[test]
    fn variables_override_the_config_file() {
        let file_values = file::parse(
            r#"
            layout = "vibrant"
            tint = false
            clock = false
            [mpd]
            host = "file.lan"
            port = 6601
            "#,
        );
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        let load = |vars| Config::from_sources(layered(env(vars), &file_values), |_| None, None);

        let cfg = load(&[]);
        assert_eq!(cfg.layout, Layout::Vibrant);
        assert!(!cfg.tint);
        assert!(!cfg.clock);
        assert_eq!(cfg.mpd_host, "file.lan");
        assert_eq!(cfg.mpd_port, 6601);
        assert!(cfg.motion);

        let cfg = load(&[
            ("ENCORE_LAYOUT", "encore-classic"),
            ("ENCORE_TINT", "on"),
            ("MPD_PORT", "6700"),
        ]);
        assert_eq!(cfg.layout, Layout::EncoreClassic);
        assert!(cfg.tint);
        assert!(!cfg.clock);
        assert_eq!(cfg.mpd_host, "file.lan");
        assert_eq!(cfg.mpd_port, 6700);

        // A legacy variable still beats the file, and an empty variable does not.
        let cfg = load(&[("MPD_DISPLAY_LAYOUT", "classic"), ("ENCORE_TINT", " ")]);
        assert_eq!(cfg.layout, Layout::Classic);
        assert!(!cfg.tint);
    }

    #[test]
    fn starter_config_gives_the_defaults() {
        let file_values = file::parse(file::STARTER);
        let from_file = Config::from_sources(layered(|_| None, &file_values), |_| None, None);
        let defaults = parse(&[]);
        assert_eq!(format!("{from_file:?}"), format!("{defaults:?}"));
    }

    #[test]
    fn source_and_mpris_player() {
        let source = |v| parse(&[("ENCORE_SOURCE", v)]).source;
        assert_eq!(source("auto"), Source::Auto);
        assert_eq!(source(" MPD "), Source::Mpd);
        assert_eq!(source("Mpris"), Source::Mpris);
        assert_eq!(source("pulse"), Source::Auto);
        assert_eq!(source(""), Source::Auto);

        let cfg = parse(&[("ENCORE_MPRIS_PLAYER", " spotify ")]);
        assert_eq!(cfg.mpris_player.as_deref(), Some("spotify"));
        assert_eq!(parse(&[("ENCORE_MPRIS_PLAYER", "  ")]).mpris_player, None);
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
            ("ENCORE_LAYOUT", "Encore-Classic"),
            ("ENCORE_LYRICS", "off"),
            ("ENCORE_LYRICS_ONLINE", "0"),
            ("MPD_MUSIC_DIR", "/srv/music"),
        ]);
        assert_eq!(cfg.layout, Layout::EncoreClassic);
        assert!(!cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
        assert_eq!(cfg.music_dir, Some(PathBuf::from("/srv/music")));

        let cfg = parse(&[
            ("ENCORE_LAYOUT", "bogus"),
            ("ENCORE_LYRICS_ONLINE", "false"),
        ]);
        assert_eq!(cfg.layout, Layout::EncoreClassic);
        assert!(cfg.lyrics_enabled);
        assert!(!cfg.lyrics_online);
    }

    #[test]
    fn online_artist_images_switch() {
        assert!(!parse(&[("ENCORE_ARTISTS_ONLINE", "0")]).artists_online);
        assert!(!parse(&[("ENCORE_ARTISTS_ONLINE", "off")]).artists_online);
        assert!(!parse(&[("MPD_DISPLAY_ARTISTS_ONLINE", "no")]).artists_online);
        let cfg = parse(&[
            ("ENCORE_ARTISTS_ONLINE", "false"),
            ("ENCORE_LYRICS_ONLINE", "on"),
        ]);
        assert!(!cfg.artists_online);
        assert!(cfg.lyrics_online);
        assert!(parse(&[("ENCORE_ARTISTS_ONLINE", "on")]).artists_online);
        assert!(parse(&[("ENCORE_LYRICS_ONLINE", "off")]).artists_online);
    }

    #[test]
    fn artist_background_defaults_and_override() {
        let cfg = parse(&[]);
        assert_eq!(cfg.artist_bg, None);
        assert!(cfg.artist_bg(Layout::EncoreClassic));
        assert!(!cfg.artist_bg(Layout::Classic));
        assert!(!cfg.artist_bg(Layout::Vibrant));

        let on = parse(&[("ENCORE_ARTIST_BG", "on")]);
        assert!([Layout::EncoreClassic, Layout::Classic, Layout::Vibrant]
            .iter()
            .all(|&l| on.artist_bg(l)));
        let off = parse(&[("ENCORE_ARTIST_BG", "OFF")]);
        assert!([Layout::EncoreClassic, Layout::Classic, Layout::Vibrant]
            .iter()
            .all(|&l| !off.artist_bg(l)));
    }

    #[test]
    fn layout_cycle() {
        assert_eq!(Layout::EncoreClassic.next(), Layout::Classic);
        assert_eq!(Layout::Classic.next(), Layout::Vibrant);
        assert_eq!(Layout::Vibrant.next(), Layout::EncoreClassic);
        for layout in [Layout::EncoreClassic, Layout::Classic, Layout::Vibrant] {
            assert_eq!(layout.next().prev(), layout);
        }
    }

    #[test]
    fn layout_names() {
        let layout = |v| parse(&[("ENCORE_LAYOUT", v)]).layout;
        assert_eq!(layout("classic"), Layout::Classic);
        assert_eq!(layout("CLASSIC"), Layout::Classic);
        assert_eq!(layout("encore-classic"), Layout::EncoreClassic);
        assert_eq!(layout("vibrant"), Layout::Vibrant);
        assert_eq!(layout("apple"), Layout::EncoreClassic);
        assert_eq!(layout("spotify"), Layout::EncoreClassic);
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
            ("ENCORE_LAYOUT", "encore-classic"),
            ("MPD_DISPLAY_LAYOUT", "vibrant"),
        ]);
        assert_eq!(cfg.layout, Layout::EncoreClassic);
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
