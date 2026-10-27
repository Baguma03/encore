//! Manually chosen lyrics and per-song timing adjustments, stored under
//! `<cache dir>/manual/`: `<key>.lrc` (synced) or `<key>.txt` (plain) for the override and
//! `<key>.json` for the adjustments, where `<key>` is [`cache_key`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::lrc::{decode_text, parse_lrc, plain_from_text};
use super::sources::cache_key;
use super::timing::Adjustments;
use super::{Lyrics, SongKey};

pub fn manual_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("manual")
}

fn path(cache_dir: &Path, key: &SongKey, ext: &str) -> PathBuf {
    manual_dir(cache_dir).join(format!("{}.{ext}", cache_key(key)))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Reads a lyrics file as text, decoding UTF-8, UTF-16 with a BOM or Latin-1.
pub fn read_lyrics_file(path: &Path) -> io::Result<String> {
    fs::read(path).map(|bytes| decode_text(&bytes))
}

/// The saved override and its file text.
pub fn read_override(cache_dir: &Path, key: &SongKey) -> Option<(Lyrics, String)> {
    for (ext, parse) in [
        ("lrc", parse_lrc as fn(&str) -> Lyrics),
        ("txt", plain_from_text),
    ] {
        let Ok(text) = read_lyrics_file(&path(cache_dir, key, ext)) else {
            continue;
        };
        let lyrics = parse(&text);
        if !lyrics.is_empty() {
            return Some((lyrics, text));
        }
    }
    None
}

pub fn has_override(cache_dir: &Path, key: &SongKey) -> bool {
    ["lrc", "txt"]
        .iter()
        .any(|ext| path(cache_dir, key, ext).is_file())
}

/// Saves `text` as the song's lyrics: `.lrc` when it has timestamps, else `.txt`. Returns
/// the parsed lyrics; text without any lyric line (only blanks or LRC tags) is rejected.
pub fn write_override(cache_dir: &Path, key: &SongKey, text: &str) -> io::Result<Lyrics> {
    let (lyrics, ext, other) = match parse_lrc(text) {
        l @ Lyrics::Synced(_) => (l, "lrc", "txt"),
        l if l.is_empty() => (l, "txt", "lrc"),
        Lyrics::Plain(_) => (plain_from_text(text), "txt", "lrc"),
    };
    if lyrics.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the lyrics are empty",
        ));
    }
    fs::create_dir_all(manual_dir(cache_dir))?;
    fs::write(path(cache_dir, key, ext), text)?;
    remove_if_present(&path(cache_dir, key, other))?;
    Ok(lyrics)
}

pub fn delete_override(cache_dir: &Path, key: &SongKey) -> io::Result<()> {
    remove_if_present(&path(cache_dir, key, "lrc"))?;
    remove_if_present(&path(cache_dir, key, "txt"))
}

/// Saved adjustments; defaults when none are saved or the file is unreadable.
pub fn read_adjustments(cache_dir: &Path, key: &SongKey) -> Adjustments {
    let path = path(cache_dir, key, "json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            log::warn!("ignoring {}: {e}", path.display());
            Adjustments::default()
        }),
        Err(_) => Adjustments::default(),
    }
}

/// Saves adjustments; default adjustments remove the file.
pub fn write_adjustments(cache_dir: &Path, key: &SongKey, adj: Adjustments) -> io::Result<()> {
    let path = path(cache_dir, key, "json");
    if adj.is_default() {
        return remove_if_present(&path);
    }
    fs::create_dir_all(manual_dir(cache_dir))?;
    let json = serde_json::to_string_pretty(&adj).map_err(io::Error::other)?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::sources::tests::temp_dir;

    fn key() -> SongKey {
        SongKey {
            file: "a.flac".into(),
            artist: "Artist".into(),
            title: "Song".into(),
            album: String::new(),
            duration: None,
        }
    }

    #[test]
    fn override_round_trip() {
        let dir = temp_dir("manual");
        let k = key();
        assert!(read_override(&dir, &k).is_none());
        assert!(!has_override(&dir, &k));

        let synced = write_override(&dir, &k, "[ar:A]\n[00:01.00]one").unwrap();
        assert!(matches!(synced, Lyrics::Synced(_)));
        assert!(dir.join("manual/artist - song.lrc").is_file());
        let (l, text) = read_override(&dir, &k).unwrap();
        assert_eq!(l, synced);
        assert_eq!(text, "[ar:A]\n[00:01.00]one");

        // Plain text replaces the synced file.
        let plain = write_override(&dir, &k, "one\n[Chorus]\ntwo").unwrap();
        assert_eq!(
            plain,
            Lyrics::Plain(vec!["one".into(), "[Chorus]".into(), "two".into()])
        );
        assert!(!dir.join("manual/artist - song.lrc").exists());
        assert_eq!(read_override(&dir, &k).unwrap().0, plain);

        assert!(write_override(&dir, &k, " \n[ar:x]\n").is_err());
        assert!(has_override(&dir, &k));
        delete_override(&dir, &k).unwrap();
        assert!(!has_override(&dir, &k));
        delete_override(&dir, &k).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn adjustments_round_trip() {
        let dir = temp_dir("adjust");
        let k = key();
        assert_eq!(read_adjustments(&dir, &k), Adjustments::default());
        let adj = Adjustments {
            offset_ms: 750,
            stretch: true,
        };
        write_adjustments(&dir, &k, adj).unwrap();
        assert_eq!(read_adjustments(&dir, &k), adj);
        write_adjustments(&dir, &k, Adjustments::default()).unwrap();
        assert!(!dir.join("manual/artist - song.json").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
