//! Files dragged onto the window, filtered by extension.

use std::path::{Path, PathBuf};

/// The paths of files dropped on the window this frame whose extension is one of
/// `extensions` (without the dot, compared case-insensitively).
pub fn dropped_files(ctx: &egui::Context, extensions: &[&str]) -> Vec<PathBuf> {
    ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path())
            .filter(|p| has_extension(p, extensions))
            .map(Path::to_path_buf)
            .collect()
    })
}

/// Whether files are being dragged over the window that could be dropped: at least one has
/// one of `extensions`, or its path is not known yet, as some platforms only report it on drop.
pub fn files_hovered(ctx: &egui::Context, extensions: &[&str]) -> bool {
    ctx.input(|i| {
        i.raw.hovered_files.iter().any(|f| {
            f.path
                .as_deref()
                .is_none_or(|p| has_extension(p, extensions))
        })
    })
}

/// Whether `path` ends in one of `extensions`, ignoring case.
pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LYRICS: [&str; 2] = ["lrc", "txt"];

    #[test]
    fn extensions_match_case_insensitively() {
        assert!(has_extension(Path::new("/m/a.lrc"), &LYRICS));
        assert!(has_extension(Path::new("/m/a.LRC"), &LYRICS));
        assert!(!has_extension(Path::new("/m/a.flac"), &LYRICS));
        assert!(!has_extension(Path::new("/m/lrc"), &LYRICS));
    }

    #[derive(Debug)]
    struct File(PathBuf);

    impl egui::DroppedFile for File {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
    }

    fn dropped(path: &str) -> egui::DroppedFileHandle {
        std::sync::Arc::new(File(PathBuf::from(path)))
    }

    fn frame(ctx: &egui::Context, input: egui::RawInput) {
        ctx.run_ui(input, |_| {}).textures_delta.clear();
    }

    #[test]
    fn dropped_and_hovered_files_are_filtered() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            dropped_files: vec![dropped("/m/a.lrc"), dropped("/m/a.flac")],
            hovered_files: vec![egui::HoveredFile {
                path: Some(PathBuf::from("/m/a.flac")),
                ..Default::default()
            }],
            ..Default::default()
        };
        frame(&ctx, input);
        assert_eq!(
            dropped_files(&ctx, &LYRICS),
            vec![PathBuf::from("/m/a.lrc")]
        );
        assert!(!files_hovered(&ctx, &LYRICS));

        let input = egui::RawInput {
            hovered_files: vec![egui::HoveredFile::default()],
            ..Default::default()
        };
        frame(&ctx, input);
        assert!(files_hovered(&ctx, &LYRICS));
        assert!(dropped_files(&ctx, &LYRICS).is_empty());
    }
}
