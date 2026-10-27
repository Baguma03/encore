//! Per-song timing adjustments applied to synced lyrics.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{LyricLine, Lyrics};

/// Stretch factors outside this range come from a `[length:]` tag that does not describe the
/// same recording and are ignored.
const STRETCH_RANGE: std::ops::RangeInclusive<f64> = 0.25..=4.0;

/// Timing adjustments for one song, stored as JSON next to its manual override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjustments {
    /// Shift in milliseconds; positive shows every line later, negative earlier. Applied on top
    /// of the file's own `[offset:]` tag.
    pub offset_ms: i64,
    /// Scales line times by `track duration / [length:]`, for sped-up or slowed versions of
    /// the recording the lyrics were timed against. Has no effect without a `[length:]` tag
    /// or a known track duration.
    pub stretch: bool,
}

impl Adjustments {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Factor that maps lyric times onto a track of length `track`, when the lyrics were timed
/// against a recording of length `length`.
pub fn stretch_factor(length: Option<Duration>, track: Option<Duration>) -> Option<f64> {
    let (length, track) = (length?, track?);
    if length.is_zero() || track.is_zero() {
        return None;
    }
    let factor = track.as_secs_f64() / length.as_secs_f64();
    STRETCH_RANGE.contains(&factor).then_some(factor)
}

/// Maps times in a lyrics file to song positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    offset_ms: i64,
    scale: f64,
}

impl Timing {
    pub const IDENTITY: Timing = Timing {
        offset_ms: 0,
        scale: 1.0,
    };

    /// `length` is the lyrics' `[length:]` tag and `track` the song's duration.
    pub fn new(adj: Adjustments, length: Option<Duration>, track: Option<Duration>) -> Self {
        let scale = if adj.stretch {
            stretch_factor(length, track).unwrap_or(1.0)
        } else {
            1.0
        };
        Self {
            offset_ms: adj.offset_ms,
            scale,
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Song position at which a line stamped `lyric` is shown. Every display and export of
    /// synced lyrics goes through this mapping via [`Timing::apply`].
    pub fn song_time(&self, lyric: Duration) -> Duration {
        let ms = lyric.as_secs_f64() * 1000.0 * self.scale + self.offset_ms as f64;
        Duration::from_millis(ms.max(0.0).round() as u64)
    }

    /// Lyric time shown at song position `song`: the inverse of [`Timing::song_time`].
    /// Positions before the offset map to zero.
    pub fn lyric_time(&self, song: Duration) -> Duration {
        let ms = (song.as_secs_f64() * 1000.0 - self.offset_ms as f64) / self.scale;
        Duration::from_millis(ms.max(0.0).round() as u64)
    }

    /// The lyrics with every line moved to its song position. Plain lyrics are unchanged.
    pub fn apply(&self, lyrics: &Lyrics) -> Lyrics {
        match lyrics {
            Lyrics::Synced(lines) if !self.is_identity() => Lyrics::Synced(
                lines
                    .iter()
                    .map(|l| LyricLine {
                        time: self.song_time(l.time),
                        text: l.text.clone(),
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::lrc::{length_tag, parse_lrc};

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn times(l: &Lyrics) -> Vec<u64> {
        match l {
            Lyrics::Synced(v) => v.iter().map(|l| l.time.as_millis() as u64).collect(),
            Lyrics::Plain(_) => panic!("expected synced"),
        }
    }

    #[test]
    fn offset_shifts_and_clamps() {
        let t = Timing::new(
            Adjustments {
                offset_ms: 500,
                stretch: false,
            },
            None,
            None,
        );
        assert_eq!(t.song_time(ms(1000)), ms(1500));
        let t = Timing::new(
            Adjustments {
                offset_ms: -1500,
                stretch: false,
            },
            None,
            None,
        );
        assert_eq!(t.song_time(ms(1000)), ms(0));
        assert_eq!(t.song_time(ms(4000)), ms(2500));
    }

    #[test]
    fn stretch_needs_length_and_duration() {
        let adj = Adjustments {
            offset_ms: 0,
            stretch: true,
        };
        // Lyrics timed against 200 s, played on a 220 s slowed version.
        let t = Timing::new(adj, Some(ms(200_000)), Some(ms(220_000)));
        assert_eq!(t.song_time(ms(100_000)), ms(110_000));
        assert!(Timing::new(adj, None, Some(ms(220_000))).is_identity());
        assert!(Timing::new(adj, Some(ms(200_000)), None).is_identity());
        // Turned off, the tag is ignored.
        let off = Adjustments {
            stretch: false,
            ..adj
        };
        assert!(Timing::new(off, Some(ms(200_000)), Some(ms(220_000))).is_identity());
        // Absurd factors are ignored.
        assert_eq!(stretch_factor(Some(ms(10_000)), Some(ms(200_000))), None);
    }

    #[test]
    fn lyric_time_inverts_song_time() {
        let t = Timing::new(
            Adjustments {
                offset_ms: 250,
                stretch: true,
            },
            Some(ms(100_000)),
            Some(ms(50_000)),
        );
        assert_eq!(t.lyric_time(ms(5250)), ms(10_000));
        assert_eq!(t.lyric_time(t.song_time(ms(42_120))), ms(42_120));
        assert_eq!(t.lyric_time(ms(100)), ms(0));
        assert_eq!(Timing::IDENTITY.lyric_time(ms(1234)), ms(1234));
    }

    #[test]
    fn stretch_then_offset() {
        let t = Timing::new(
            Adjustments {
                offset_ms: 250,
                stretch: true,
            },
            Some(ms(100_000)),
            Some(ms(50_000)),
        );
        assert_eq!(t.song_time(ms(10_000)), ms(5250));
    }

    #[test]
    fn apply_combines_with_offset_tag() {
        // `[offset:+500]` shows lines 500 ms earlier; the adjustment then delays them 1 s.
        let text = "[offset:+500]\n[length:00:10.00]\n[00:02.00]a\n[00:04.00]b";
        let lyrics = parse_lrc(text);
        assert_eq!(times(&lyrics), vec![1500, 3500]);
        let t = Timing::new(
            Adjustments {
                offset_ms: 1000,
                stretch: true,
            },
            length_tag(text),
            Some(ms(20_000)),
        );
        assert_eq!(times(&t.apply(&lyrics)), vec![4000, 8000]);
        let plain = Lyrics::Plain(vec!["x".into()]);
        assert_eq!(t.apply(&plain), plain);
    }

    #[test]
    fn adjustments_json() {
        let a: Adjustments = serde_json::from_str(r#"{"offset_ms": -300}"#).unwrap();
        assert_eq!(
            a,
            Adjustments {
                offset_ms: -300,
                stretch: false
            }
        );
        let back: Adjustments = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
        assert!(Adjustments::default().is_default());
    }
}
