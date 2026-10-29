//! Folders of user-provided files under the XDG data directory, and of downloads under the XDG
//! cache directory.

use std::path::PathBuf;

/// `<data dir>/encore/artists`: artist background images. `./artists` when there is no data
/// directory.
pub fn artists_dir() -> PathBuf {
    data_subdir("artists")
}

/// `<data dir>/encore/lyrics`: local `<artist> - <title>.lrc` files. `./lyrics` when there is
/// no data directory.
pub fn lyrics_dir() -> PathBuf {
    data_subdir("lyrics")
}

/// `<data dir>/encore/lyrics/manual`: lyrics saved in the lyrics manager and per-song timing
/// adjustments. `./lyrics/manual` when there is no data directory. The local lyrics lookup
/// only considers `.lrc`/`.txt` files directly inside [`lyrics_dir`], so it never reads this
/// folder.
pub fn manual_lyrics_dir() -> PathBuf {
    lyrics_dir().join("manual")
}

/// `<cache dir>/encore/artists`: downloaded artist background images and not-found markers.
/// `None` when there is no cache directory.
pub fn artist_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("encore").join("artists"))
}

fn data_subdir(name: &str) -> PathBuf {
    dirs::data_dir().map_or_else(
        || PathBuf::from(".").join(name),
        |d| d.join("encore").join(name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_live_under_the_data_dir() {
        let Some(data) = dirs::data_dir() else {
            assert_eq!(artists_dir(), PathBuf::from("./artists"));
            return;
        };
        assert_eq!(artists_dir(), data.join("encore/artists"));
        assert_eq!(lyrics_dir(), data.join("encore/lyrics"));
        assert_eq!(manual_lyrics_dir(), data.join("encore/lyrics/manual"));
        if let Some(cache) = dirs::cache_dir() {
            assert_eq!(artist_cache_dir(), Some(cache.join("encore/artists")));
        }
    }
}
