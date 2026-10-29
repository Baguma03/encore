//! The two font roles. The interface font, the bundled Lexend Deca, sets panels, overlays,
//! notifications and every other piece of egui chrome. The display font sets the now playing
//! display and exported clips; it defaults to the bundled Figtree and can be any installed font
//! family or font file, switched while running.
//!
//! Each role has its own egui font families, so switching the display font only replaces the
//! font data of the display families. egui's default fonts stay behind every family as glyph
//! fallbacks (emoji, symbols and scripts the chosen fonts lack).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};

use egui::epaint::text::VariationCoords;
use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use log::{info, warn};

/// Font data and family of the interface's regular face; it heads the proportional family.
pub const INTERFACE_REGULAR: &str = "interface-regular";
/// Font data and family name of the interface's bold face.
pub const INTERFACE_BOLD: &str = "bold";
/// Font data of the bundled Figtree behind the interface font, for the glyphs it lacks.
const INTERFACE_FALLBACK: &str = "interface-fallback";
const INTERFACE_FALLBACK_BOLD: &str = "interface-fallback-bold";
/// Font data and family name of the display font's regular face.
pub const DISPLAY_REGULAR: &str = "display";
/// Font data and family name of the display font's bold face.
pub const DISPLAY_BOLD: &str = "display-bold";

/// Name of the bundled display font, which needs no lookup.
pub const BUNDLED_NAME: &str = "Figtree";

/// Weights set on a variable font's `wght` axis for the regular and bold faces.
const REGULAR_WEIGHT: f32 = 400.0;
const BOLD_WEIGHT: f32 = 700.0;

/// The faces of a display font, ready to install.
#[derive(Clone)]
pub struct DisplayFont {
    name: String,
    regular: Arc<FontData>,
    bold: Arc<FontData>,
}

impl DisplayFont {
    /// The bundled Figtree.
    pub fn bundled() -> Self {
        Self {
            name: BUNDLED_NAME.to_owned(),
            regular: Arc::new(FontData::from_static(include_bytes!(
                "../../assets/font/Figtree-Regular.ttf"
            ))),
            bold: Arc::new(FontData::from_static(include_bytes!(
                "../../assets/font/Figtree-Bold.ttf"
            ))),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Default for DisplayFont {
    fn default() -> Self {
        Self::bundled()
    }
}

impl std::fmt::Debug for DisplayFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DisplayFont").field(&self.name).finish()
    }
}

/// The font definitions of both roles, with `display` as the display font.
pub fn definitions(display: &DisplayFont) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    // egui's own proportional fonts, kept behind every family as fallbacks.
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let bundled = DisplayFont::bundled();
    let data = [
        (
            INTERFACE_REGULAR,
            Arc::new(FontData::from_static(include_bytes!(
                "../../assets/font/LexendDeca-Regular.ttf"
            ))),
        ),
        (
            INTERFACE_BOLD,
            Arc::new(FontData::from_static(include_bytes!(
                "../../assets/font/LexendDeca-Bold.ttf"
            ))),
        ),
        (INTERFACE_FALLBACK, bundled.regular),
        (INTERFACE_FALLBACK_BOLD, bundled.bold),
        (DISPLAY_REGULAR, Arc::clone(&display.regular)),
        (DISPLAY_BOLD, Arc::clone(&display.bold)),
    ];
    for (name, data) in data {
        fonts.font_data.insert(name.to_owned(), data);
    }
    // Lexend Deca lacks arrows, which the interface's key caps show; Figtree has them and
    // matches it more closely than egui's fallbacks.
    let families = [
        (
            FontFamily::Proportional,
            &[INTERFACE_REGULAR, INTERFACE_FALLBACK][..],
        ),
        (
            FontFamily::Name(INTERFACE_BOLD.into()),
            &[INTERFACE_BOLD, INTERFACE_FALLBACK_BOLD],
        ),
        (FontFamily::Name(DISPLAY_REGULAR.into()), &[DISPLAY_REGULAR]),
        (FontFamily::Name(DISPLAY_BOLD.into()), &[DISPLAY_BOLD]),
    ];
    for (family, faces) in families {
        let mut chain: Vec<String> = faces.iter().map(|f| (*f).to_owned()).collect();
        chain.extend(fallbacks.iter().cloned());
        fonts.families.insert(family, chain);
    }
    fonts
}

/// Installs both roles' fonts, with `display` as the display font. Text laid out after this
/// call uses them; otherwise they take over from the next frame.
pub fn install(ctx: &egui::Context, display: &DisplayFont) {
    ctx.set_fonts(definitions(display));
}

/// What an entry of the display font list names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSpec {
    /// The bundled Figtree.
    Bundled,
    /// The family name of an installed font, looked up with fontconfig.
    Family(String),
    /// A font file: its regular face, with a bold sibling when there is one.
    File(PathBuf),
}

impl FontSpec {
    /// Reads a list entry: "Figtree" (any case) is the bundled font, a path to a font file
    /// (ending in `.ttf`, `.otf`, `.ttc` or `.otc`, or starting with `/`, `~/` or `./`) a
    /// file with `~` expanded against `home`, and anything else a family name.
    pub fn parse(entry: &str, home: Option<&Path>) -> Self {
        let entry = entry.trim();
        let lower = entry.to_lowercase();
        let font_file = [".ttf", ".otf", ".ttc", ".otc"]
            .iter()
            .any(|ext| lower.ends_with(ext));
        let path_like = ["/", "~/", "./"].iter().any(|p| entry.starts_with(p));
        if entry.eq_ignore_ascii_case(BUNDLED_NAME) {
            Self::Bundled
        } else if font_file || path_like {
            let path = match (entry.strip_prefix("~/"), home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(entry),
            };
            Self::File(path)
        } else {
            Self::Family(entry.to_owned())
        }
    }

    /// The name shown when switching to the font: the family, or a file's name without its
    /// extension and a trailing "Regular".
    pub fn display_name(&self) -> String {
        match self {
            Self::Bundled => BUNDLED_NAME.to_owned(),
            Self::Family(name) => name.clone(),
            Self::File(path) => {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let trimmed = stem
                    .strip_suffix("Regular")
                    .or_else(|| stem.strip_suffix("regular"))
                    .map(|s| s.trim_end_matches(['-', '_', ' ']))
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&stem);
                trimmed.to_owned()
            }
        }
    }
}

/// Where the bold face of a regular font file would be: the same name with its last "Regular"
/// (or "regular") replaced by "Bold" (or "bold"). `None` when the name has neither.
pub fn bold_sibling(regular: &Path) -> Option<PathBuf> {
    let name = regular.file_name()?.to_str()?;
    let (at, bold) = [("Regular", "Bold"), ("regular", "bold")]
        .iter()
        .filter_map(|(from, to)| name.rfind(from).map(|at| ((at, from.len()), *to)))
        .max_by_key(|((at, _), _)| *at)?;
    let (start, len) = at;
    let renamed = format!("{}{bold}{}", &name[..start], &name[start + len..]);
    Some(regular.with_file_name(renamed))
}

/// The `fc-match` output format: file, face index and family names, one per line.
const FC_FORMAT: &str = r"%{file}\n%{index}\n%{family}\n";

/// A font fontconfig picked for a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FcMatch {
    pub file: PathBuf,
    /// Face index within the file; for a variable font the named instance sits in the upper
    /// 16 bits.
    pub index: u32,
    /// Every family name of the font, localised names included.
    pub families: Vec<String>,
}

impl FcMatch {
    /// Whether one of the font's family names is `wanted`, ignoring case. fontconfig always
    /// returns some font, so this tells a real match from a substitute.
    pub fn is_family(&self, wanted: &str) -> bool {
        let wanted = wanted.trim().to_lowercase();
        self.families
            .iter()
            .any(|f| f.trim().to_lowercase() == wanted)
    }

    /// The face within the file, without a variable font's named instance.
    fn face_index(&self) -> u32 {
        self.index & 0xFFFF
    }
}

/// Reads `fc-match -f` output in [`FC_FORMAT`]. `None` when it names no file.
pub fn parse_fc_match(output: &str) -> Option<FcMatch> {
    let mut lines = output.lines();
    let file = lines.next()?.trim();
    if file.is_empty() {
        return None;
    }
    let index = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0);
    let families = lines.next().map(split_families).unwrap_or_default();
    Some(FcMatch {
        file: PathBuf::from(file),
        index,
        families,
    })
}

/// Splits fontconfig's comma-separated family list, undoing its backslash escapes.
fn split_families(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => current.extend(chars.next()),
            ',' => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    out.retain(|f| !f.trim().is_empty());
    out
}

/// A fontconfig pattern for `family` with `property` (such as `style=Bold`), escaping the
/// characters fontconfig gives a meaning in family names.
pub fn fc_pattern(family: &str, property: &str) -> String {
    let mut out = String::new();
    for c in family.trim().chars() {
        if matches!(c, '\\' | '-' | ':' | ',') {
            out.push('\\');
        }
        out.push(c);
    }
    format!("{out}:{property}")
}

/// Why a family could not be looked up.
#[derive(Debug)]
enum LookupError {
    /// `fc-match` could not be run.
    NoFontconfig,
    /// Fontconfig has no such family; the font it substitutes, if any.
    NotInstalled(Option<String>),
}

/// Runs `fc-match` for `family` with `property`; only a font of that family counts.
fn fc_match(family: &str, property: &str) -> Result<FcMatch, LookupError> {
    let output = Command::new("fc-match")
        .arg("-f")
        .arg(FC_FORMAT)
        .arg(fc_pattern(family, property))
        .output()
        .map_err(|_| LookupError::NoFontconfig)?;
    let found = output
        .status
        .success()
        .then(|| parse_fc_match(&String::from_utf8_lossy(&output.stdout)))
        .flatten();
    match found {
        Some(m) if m.is_family(family) => Ok(m),
        Some(m) => Err(LookupError::NotInstalled(m.families.first().cloned())),
        None => Err(LookupError::NotInstalled(None)),
    }
}

/// Resolves and loads the font `spec` names; the error says why it cannot be used. Looks
/// installed families up with `fc-match` and reads font files, so it is best kept off the UI
/// thread.
pub fn resolve(spec: &FontSpec) -> Result<DisplayFont, String> {
    let name = spec.display_name();
    let (regular, bold) = match spec {
        FontSpec::Bundled => return Ok(DisplayFont::bundled()),
        FontSpec::Family(family) => {
            let regular = fc_match(family, "style=Regular").map_err(|e| match e {
                LookupError::NoFontconfig => {
                    "fc-match is not installed, so only Figtree and font files work".to_owned()
                }
                LookupError::NotInstalled(Some(other)) => {
                    format!("no installed font family is named {family:?} (fontconfig offers {other:?})")
                }
                LookupError::NotInstalled(None) => {
                    format!("no installed font family is named {family:?}")
                }
            })?;
            let bold = ["style=Bold", "weight=bold"]
                .iter()
                .find_map(|property| fc_match(family, property).ok());
            let bold = bold.map(|m| (m.file.clone(), m.face_index()));
            ((regular.file.clone(), regular.face_index()), bold)
        }
        FontSpec::File(path) => {
            let bold = bold_sibling(path).filter(|p| p.is_file()).map(|p| (p, 0));
            ((path.clone(), 0), bold)
        }
    };
    let regular_bytes = read_font(&regular.0, regular.1)?;
    let regular_data = with_weight(
        FontData::from_blob(Arc::clone(&regular_bytes), regular.1),
        REGULAR_WEIGHT,
    );
    let bold_data = match bold {
        // A missing or broken bold face falls back to the regular one.
        Some((path, index)) if path == regular.0 => Some((Arc::clone(&regular_bytes), index)),
        Some((path, index)) => match read_font(&path, index) {
            Ok(bytes) => Some((bytes, index)),
            Err(e) => {
                warn!("display font {name}: ignoring its bold face: {e}");
                None
            }
        },
        None => None,
    }
    .map_or_else(
        || FontData::from_blob(Arc::clone(&regular_bytes), regular.1),
        |(bytes, index)| FontData::from_blob(bytes, index),
    );
    Ok(DisplayFont {
        name,
        regular: Arc::new(regular_data),
        bold: Arc::new(with_weight(bold_data, BOLD_WEIGHT)),
    })
}

/// The bytes of the font file at `path`, checked to hold an OpenType face at `index`.
fn read_font(path: &Path, index: u32) -> Result<egui::epaint::text::Blob, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    check_sfnt(&bytes, index).map_err(|e| format!("{} {e}", path.display()))?;
    Ok(Arc::new(bytes))
}

/// Sets a variable font's weight axis to `weight`, within the axis's range, so the regular and
/// bold faces of one variable file differ. Static fonts are left as they are.
fn with_weight(data: FontData, weight: f32) -> FontData {
    let Some(axis) = data
        .variation_axes()
        .into_iter()
        .find(|a| a.tag.to_be_bytes() == *b"wght")
    else {
        return data;
    };
    let weight = weight.clamp(axis.range.min, axis.range.max);
    data.tweak(FontTweak {
        coords: VariationCoords::new([(b"wght", weight)]),
        ..Default::default()
    })
}

/// Checks that `bytes` is an OpenType or TrueType font, or a collection of them, whose face
/// `index` has the tables text layout needs. egui cannot draw with a face that fails this and
/// stops at one it cannot parse, so a font is checked before it is installed.
pub fn check_sfnt(bytes: &[u8], index: u32) -> Result<(), String> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let start = if bytes.starts_with(b"ttcf") {
        let count = u32_at(8).ok_or("is a truncated font collection")?;
        if index >= count {
            return Err(format!("has no face {index}"));
        }
        u32_at(12 + 4 * index as usize).ok_or("is a truncated font collection")? as usize
    } else if index == 0 {
        0
    } else {
        return Err(format!("has no face {index}"));
    };
    let version = bytes
        .get(start..start + 4)
        .ok_or("is not a font file (too short)")?;
    if ![&[0, 1, 0, 0][..], b"OTTO", b"true"].contains(&version) {
        return Err("is not a TrueType or OpenType font".to_owned());
    }
    let tables = u16_at(start + 4).ok_or("is a truncated font")?;
    let mut found = Vec::new();
    for i in 0..usize::from(tables) {
        let record = start + 12 + 16 * i;
        let tag = bytes.get(record..record + 4).ok_or("is a truncated font")?;
        let offset = u32_at(record + 8).ok_or("is a truncated font")? as usize;
        let length = u32_at(record + 12).ok_or("is a truncated font")? as usize;
        if offset
            .checked_add(length)
            .is_none_or(|end| end > bytes.len())
        {
            return Err("is a truncated font".to_owned());
        }
        found.push(tag);
    }
    for needed in [b"cmap", b"head", b"hhea", b"hmtx", b"maxp"] {
        if !found.contains(&&needed[..]) {
            let tag = String::from_utf8_lossy(needed);
            return Err(format!("is not a usable font (no {tag} table)"));
        }
    }
    Ok(())
}

/// The next entry of `len` list entries after `from`, going backwards unless `forward`,
/// wrapping around and passing over the entries that are not `usable`. `None` when no other
/// entry is usable.
pub fn step_index(
    len: usize,
    from: usize,
    forward: bool,
    usable: impl Fn(usize) -> bool,
) -> Option<usize> {
    (1..len)
        .map(|k| {
            if forward {
                (from + k) % len
            } else {
                (from + len - k) % len
            }
        })
        .find(|&i| usable(i))
}

/// A list entry's font: still being looked up, ready, or unusable.
enum Slot {
    Pending,
    Ready(DisplayFont),
    Failed,
}

/// What a step through the list or a finished lookup did.
#[derive(Debug)]
pub enum Change {
    /// Switched to this font; install it.
    Switched(DisplayFont),
    /// Moved to a font whose lookup is still running; it is installed once it is ready.
    Loading(String),
    /// The list holds no other usable font.
    OnlyOne(String),
    /// The font moved to turned out unusable; the shown one stays.
    Failed(String),
}

/// The display fonts `u` cycles through, looked up on a background thread.
pub struct DisplayFonts {
    names: Vec<String>,
    slots: Vec<Slot>,
    /// The entry chosen last.
    selected: usize,
    /// The entry whose font is installed.
    shown: usize,
    results: Option<mpsc::Receiver<(usize, Result<DisplayFont, String>)>>,
}

impl DisplayFonts {
    /// Loads `entries[start]` right away, falling back to the bundled Figtree when it cannot be
    /// used, and looks the other entries up on a background thread, calling `wake` as each one
    /// is done.
    pub fn new(
        entries: &[String],
        start: usize,
        home: Option<&Path>,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let mut specs: Vec<FontSpec> = entries.iter().map(|e| FontSpec::parse(e, home)).collect();
        if specs.is_empty() {
            specs.push(FontSpec::Bundled);
        }
        let mut start = start.min(specs.len() - 1);
        let mut slots: Vec<Slot> = specs.iter().map(|_| Slot::Pending).collect();
        match resolve(&specs[start]) {
            Ok(font) => slots[start] = Slot::Ready(font),
            Err(e) => {
                warn!(
                    "display font {}: {e}; using {BUNDLED_NAME}",
                    specs[start].display_name()
                );
                slots[start] = Slot::Failed;
                start = match specs.iter().position(|s| *s == FontSpec::Bundled) {
                    Some(i) => i,
                    None => {
                        specs.insert(start, FontSpec::Bundled);
                        slots.insert(start, Slot::Pending);
                        start
                    }
                };
                slots[start] = Slot::Ready(DisplayFont::bundled());
            }
        }
        let names = specs.iter().map(FontSpec::display_name).collect();
        let pending: Vec<(usize, FontSpec)> = specs
            .into_iter()
            .enumerate()
            .filter(|(i, _)| matches!(slots[*i], Slot::Pending))
            .collect();
        let results = (!pending.is_empty()).then(|| {
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("display-fonts".into())
                .spawn(move || {
                    for (i, spec) in pending {
                        if tx.send((i, resolve(&spec))).is_err() {
                            return;
                        }
                        wake();
                    }
                });
            if let Err(e) = spawned {
                warn!("cannot look the display fonts up: {e}");
            }
            rx
        });
        Self {
            names,
            slots,
            selected: start,
            shown: start,
            results,
        }
    }

    /// The installed display font.
    pub fn current(&self) -> DisplayFont {
        match &self.slots[self.shown] {
            Slot::Ready(font) => font.clone(),
            _ => DisplayFont::bundled(),
        }
    }

    /// Takes in finished lookups; reports a switch when the selected font has become ready, or
    /// a failure when it turned out unusable.
    pub fn poll(&mut self) -> Option<Change> {
        let results: Vec<_> = self.results.as_ref()?.try_iter().collect();
        let mut change = None;
        for (i, result) in results {
            self.slots[i] = match result {
                Ok(font) => {
                    info!("display font {} is ready", self.names[i]);
                    Slot::Ready(font)
                }
                Err(e) => {
                    warn!("display font {}: {e}", self.names[i]);
                    Slot::Failed
                }
            };
            if i == self.selected && i != self.shown {
                change = Some(match &self.slots[i] {
                    Slot::Ready(font) => {
                        self.shown = i;
                        Change::Switched(font.clone())
                    }
                    _ => {
                        self.selected = self.shown;
                        Change::Failed(self.names[i].clone())
                    }
                });
            }
        }
        change
    }

    /// Moves to the next usable font in the list, or the previous one unless `forward`.
    pub fn step(&mut self, forward: bool) -> Change {
        let usable = |i: usize| !matches!(self.slots[i], Slot::Failed);
        let Some(i) = step_index(self.slots.len(), self.selected, forward, usable) else {
            return Change::OnlyOne(self.names[self.shown].clone());
        };
        self.selected = i;
        match &self.slots[i] {
            Slot::Ready(font) => {
                self.shown = i;
                Change::Switched(font.clone())
            }
            _ => Change::Loading(self.names[i].clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_entries() {
        let home = Path::new("/home/u");
        let parse = |e| FontSpec::parse(e, Some(home));
        assert_eq!(parse("Figtree"), FontSpec::Bundled);
        assert_eq!(parse(" figtree "), FontSpec::Bundled);
        assert_eq!(parse("Inter"), FontSpec::Family("Inter".into()));
        assert_eq!(parse("DejaVu Sans"), FontSpec::Family("DejaVu Sans".into()));
        assert_eq!(
            parse("~/fonts/Poppins-Regular.ttf"),
            FontSpec::File("/home/u/fonts/Poppins-Regular.ttf".into())
        );
        assert_eq!(
            parse("/usr/share/fonts/X.OTF"),
            FontSpec::File("/usr/share/fonts/X.OTF".into())
        );
        assert_eq!(parse("Font.ttc"), FontSpec::File("Font.ttc".into()));
        assert_eq!(parse("./fonts/x"), FontSpec::File("./fonts/x".into()));
        assert_eq!(
            FontSpec::parse("~/a.ttf", None),
            FontSpec::File("~/a.ttf".into())
        );
    }

    #[test]
    fn display_names() {
        let name = |e| FontSpec::parse(e, None).display_name();
        assert_eq!(name("figtree"), "Figtree");
        assert_eq!(name("Inter"), "Inter");
        assert_eq!(name("/f/Poppins-Regular.ttf"), "Poppins");
        assert_eq!(name("/f/Lato_regular.otf"), "Lato");
        assert_eq!(name("/f/Regular.ttf"), "Regular");
        assert_eq!(name("/f/Mono.ttf"), "Mono");
    }

    #[test]
    fn bold_siblings() {
        let bold = |p: &str| bold_sibling(Path::new(p));
        assert_eq!(
            bold("/f/Poppins-Regular.ttf"),
            Some("/f/Poppins-Bold.ttf".into())
        );
        assert_eq!(bold("/f/lato_regular.otf"), Some("/f/lato_bold.otf".into()));
        // Only the file name changes, and only its last "Regular".
        assert_eq!(
            bold("/Regular/RegularSans-Regular.ttf"),
            Some("/Regular/RegularSans-Bold.ttf".into())
        );
        assert_eq!(bold("/f/Mono.ttf"), None);
    }

    #[test]
    fn fc_match_output() {
        let m = parse_fc_match("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf\n0\nDejaVu Sans\n")
            .expect("match");
        assert_eq!(
            m.file,
            PathBuf::from("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf")
        );
        assert_eq!(m.index, 0);
        assert!(m.is_family("dejavu sans"));
        assert!(!m.is_family("DejaVu"));

        // Several family names, escapes and a variable font's named instance.
        let m = parse_fc_match(
            "/f/Oktah.otf\n458752\nFONTSPRING DEMO \\- Oktah Neue,Oktah Neue,Big\\, Bold\n",
        )
        .expect("match");
        assert_eq!(
            m.families,
            ["FONTSPRING DEMO - Oktah Neue", "Oktah Neue", "Big, Bold"]
        );
        assert!(m.is_family("Oktah Neue"));
        assert!(m.is_family("big, bold"));
        assert_eq!(m.face_index(), 0);

        // fontconfig substitutes another family for one that is not installed.
        let m = parse_fc_match("/f/NotoSans-Regular.ttf\n0\nNoto Sans\n").expect("match");
        assert!(!m.is_family("Inter"));

        assert_eq!(parse_fc_match(""), None);
        assert_eq!(parse_fc_match("\n0\nX\n"), None);
        let bare = parse_fc_match("/f/x.ttf").expect("match");
        assert_eq!((bare.index, bare.families.len()), (0, 0));
    }

    #[test]
    fn fc_patterns() {
        assert_eq!(fc_pattern("Inter", "style=Bold"), "Inter:style=Bold");
        assert_eq!(
            fc_pattern(" Foo-Bar: Baz ", "weight=bold"),
            r"Foo\-Bar\: Baz:weight=bold"
        );
    }

    #[test]
    fn steps_wrap_both_ways_and_skip_unusable_entries() {
        let all = |_| true;
        assert_eq!(step_index(3, 0, true, all), Some(1));
        assert_eq!(step_index(3, 2, true, all), Some(0));
        assert_eq!(step_index(3, 0, false, all), Some(2));
        assert_eq!(step_index(3, 1, false, all), Some(0));
        assert_eq!(step_index(1, 0, true, all), None);
        assert_eq!(step_index(1, 0, false, all), None);
        let not_one = |i| i != 1;
        assert_eq!(step_index(3, 0, true, not_one), Some(2));
        assert_eq!(step_index(3, 2, false, not_one), Some(0));
        assert_eq!(step_index(2, 0, true, not_one), None);
    }

    #[test]
    fn cycling_through_bundled_and_unusable_fonts() {
        let entries = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let missing = "/nonexistent/Missing-Regular.ttf";
        let mut fonts = DisplayFonts::new(&entries(&["Figtree", missing]), 0, None, || {});
        assert_eq!(fonts.current().name(), "Figtree");
        // The missing file's lookup runs in the background; moving to it waits for it.
        assert!(matches!(fonts.step(true), Change::Loading(n) if n == "Missing"));
        let change = loop {
            if let Some(change) = fonts.poll() {
                break change;
            }
            std::thread::yield_now();
        };
        assert!(matches!(change, Change::Failed(n) if n == "Missing"));
        assert_eq!(fonts.current().name(), "Figtree");
        assert!(matches!(fonts.step(false), Change::OnlyOne(n) if n == "Figtree"));

        // An unusable startup font falls back to Figtree, added to the list when missing.
        let fonts = DisplayFonts::new(&entries(&[missing]), 0, None, || {});
        assert_eq!(fonts.current().name(), "Figtree");
        assert_eq!(fonts.names, ["Figtree", "Missing"]);
    }

    #[test]
    fn sfnt_check() {
        let figtree = include_bytes!("../../assets/font/Figtree-Regular.ttf");
        assert_eq!(check_sfnt(figtree, 0), Ok(()));
        assert!(check_sfnt(figtree, 1).is_err());
        assert!(check_sfnt(b"not a font at all", 0).is_err());
        assert!(check_sfnt(&figtree[..2000], 0).is_err());
        // A one-face collection wrapping the font at offset 16.
        let mut ttc = b"ttcf\0\x01\0\0\0\0\0\x01\0\0\0\x10".to_vec();
        let mut shifted = figtree.to_vec();
        // Table offsets are from the start of the collection.
        let tables = u16::from_be_bytes([shifted[4], shifted[5]]) as usize;
        for i in 0..tables {
            let at = 12 + 16 * i + 8;
            let offset = u32::from_be_bytes(shifted[at..at + 4].try_into().unwrap()) + 16;
            shifted[at..at + 4].copy_from_slice(&offset.to_be_bytes());
        }
        ttc.extend(shifted);
        assert_eq!(check_sfnt(&ttc, 0), Ok(()));
        assert!(check_sfnt(&ttc, 1).is_err());
    }

    #[test]
    fn both_roles_install_with_fallbacks() {
        let defs = definitions(&DisplayFont::bundled());
        for (family, first) in [
            (FontFamily::Proportional, INTERFACE_REGULAR),
            (FontFamily::Name(INTERFACE_BOLD.into()), INTERFACE_BOLD),
            (FontFamily::Name(DISPLAY_REGULAR.into()), DISPLAY_REGULAR),
            (FontFamily::Name(DISPLAY_BOLD.into()), DISPLAY_BOLD),
        ] {
            let chain = &defs.families[&family];
            assert_eq!(chain[0], first);
            let defaults = &FontDefinitions::default().families[&FontFamily::Proportional];
            assert!(
                chain.ends_with(defaults),
                "{family:?} keeps egui's fallbacks"
            );
        }
        let ctx = egui::Context::default();
        install(&ctx, &DisplayFont::bundled());
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let g = ui.painter().layout_no_wrap(
                "Aä😀".into(),
                super::super::display_bold(20.0),
                egui::Color32::WHITE,
            );
            assert!(g.size().x > 0.0);
        });
        output.textures_delta.clear();
    }

    /// Resolves a family installed on this machine through fontconfig.
    #[test]
    #[ignore = "needs fontconfig and the DejaVu Sans font"]
    fn resolves_an_installed_family() {
        let font = resolve(&FontSpec::Family("DejaVu Sans".into())).expect("DejaVu Sans");
        assert_eq!(font.name(), "DejaVu Sans");
        assert!(!Arc::ptr_eq(&font.regular.font, &font.bold.font));
        let err = resolve(&FontSpec::Family("No Such Font Family 123".into())).unwrap_err();
        assert!(err.contains("no installed font family"), "{err}");
    }
}
