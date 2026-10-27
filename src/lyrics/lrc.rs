//! LRC parsing and serialisation.

use std::time::Duration;

use super::{LyricLine, Lyrics};

/// Parses LRC text. Text without any timestamped line becomes [`Lyrics::Plain`].
pub fn parse_lrc(text: &str) -> Lyrics {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset_ms: i64 = 0;
    let mut synced: Vec<(u64, String)> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps: Vec<u64> = Vec::new();
        let mut malformed = false;
        let mut metadata = false;

        while let Some(after) = rest.strip_prefix('[') {
            let Some(end) = after.find(']') else {
                malformed = true;
                break;
            };
            let tag = &after[..end];
            if let Some(ms) = parse_timestamp(tag) {
                stamps.push(ms);
            } else if stamps.is_empty() && is_metadata(tag) {
                metadata = true;
                if let Some((key, value)) = tag.split_once(':') {
                    if key.trim().eq_ignore_ascii_case("offset") {
                        if let Ok(v) = value.trim().trim_start_matches('+').parse::<i64>() {
                            offset_ms = v;
                        }
                    }
                }
                break;
            } else {
                malformed = true;
                break;
            }
            rest = after[end + 1..].trim_start();
        }

        if malformed || metadata {
            continue;
        }
        let line = clean_text(rest);
        if stamps.is_empty() {
            plain.push(line);
        } else {
            for ms in stamps {
                synced.push((ms, line.clone()));
            }
        }
    }

    if synced.is_empty() {
        return Lyrics::Plain(trim_blank_edges(plain));
    }

    synced.sort_by_key(|(ms, _)| *ms);
    Lyrics::Synced(
        synced
            .into_iter()
            .map(|(ms, text)| LyricLine {
                time: Duration::from_millis((ms as i64 - offset_ms).max(0) as u64),
                text,
            })
            .collect(),
    )
}

/// Serialises lyrics as LRC; plain lyrics are written as bare lines.
pub fn to_lrc(lyrics: &Lyrics) -> String {
    let mut out = String::new();
    match lyrics {
        Lyrics::Synced(lines) => {
            for line in lines {
                out.push('[');
                out.push_str(&format_timestamp(line.time));
                out.push(']');
                out.push_str(&line.text);
                out.push('\n');
            }
        }
        Lyrics::Plain(lines) => {
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// `mm:ss.xx`, or `mm:ss.xxx` when the time is not a whole number of centiseconds.
pub fn format_timestamp(time: Duration) -> String {
    let ms = time.as_millis();
    let (min, sec, frac) = (ms / 60_000, (ms / 1000) % 60, ms % 1000);
    if frac % 10 == 0 {
        format!("{min:02}:{sec:02}.{:02}", frac / 10)
    } else {
        format!("{min:02}:{sec:02}.{frac:03}")
    }
}

/// Value of the first `[key:value]` metadata tag named `key` (case-insensitive), trimmed.
pub fn tag_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let tag = line.trim().strip_prefix('[')?;
        let tag = &tag[..tag.find(']')?];
        let (k, v) = tag.split_once(':')?;
        (k.trim().eq_ignore_ascii_case(key) && is_metadata(tag)).then(|| v.trim())
    })
}

/// The `[length:mm:ss.xx]` tag: the duration of the recording the lyrics were timed against.
pub fn length_tag(text: &str) -> Option<Duration> {
    let ms = parse_timestamp(tag_value(text, "length")?)?;
    (ms > 0).then(|| Duration::from_millis(ms))
}

/// Decodes lyrics file contents: UTF-8 (BOM optional), UTF-16 with a BOM, else Latin-1.
pub fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| {
                if be {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
        },
    }
}

/// Plain-text lyrics: trimmed lines with leading/trailing blank lines removed.
pub(crate) fn plain_from_text(text: &str) -> Lyrics {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Lyrics::Plain(trim_blank_edges(
        text.lines().map(|l| l.trim().to_string()).collect(),
    ))
}

fn trim_blank_edges(mut lines: Vec<String>) -> Vec<String> {
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let first = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    lines.drain(..first);
    lines
}

/// `mm:ss`, `mm:ss.f{1,3}` or `mm:ss:ff` in milliseconds.
fn parse_timestamp(tag: &str) -> Option<u64> {
    let (min, rest) = tag.trim().split_once(':')?;
    let (sec, frac) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(min) || !all_digits(sec) || sec.len() > 2 {
        return None;
    }
    let min: u64 = min.parse().ok()?;
    let sec: u64 = sec.parse().ok()?;
    let frac_ms = match frac {
        None => 0,
        Some(f) if all_digits(f) => {
            let f = &f[..f.len().min(3)];
            f.parse::<u64>().ok()? * 10u64.pow(3 - f.len() as u32)
        }
        Some(_) => return None,
    };
    Some(min * 60_000 + sec * 1000 + frac_ms)
}

/// `[key:value]` with an alphabetic key, e.g. `[ar:...]`, `[offset:+100]`.
fn is_metadata(tag: &str) -> bool {
    tag.split_once(':').is_some_and(|(key, _)| {
        let key = key.trim();
        !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphabetic() || b == b'#')
    })
}

/// Strips enhanced word tags (`<mm:ss.xx>`) and collapses whitespace.
fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('>') {
            Some(end) if parse_timestamp(&after[..end]).is_some() => rest = &after[end + 1..],
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced(l: &Lyrics) -> Vec<(u64, &str)> {
        match l {
            Lyrics::Synced(lines) => lines
                .iter()
                .map(|l| (l.time.as_millis() as u64, l.text.as_str()))
                .collect(),
            Lyrics::Plain(_) => panic!("expected synced, got {l:?}"),
        }
    }

    #[test]
    fn basic() {
        let l = parse_lrc("[00:01.00]  Hello  \n[00:02.50]World\n");
        assert_eq!(synced(&l), vec![(1000, "Hello"), (2500, "World")]);
    }

    #[test]
    fn precision_variants() {
        let l = parse_lrc("[00:01]a\n[00:02.5]b\n[00:03.25]c\n[00:04.125]d\n[01:05:50]e");
        assert_eq!(
            synced(&l),
            vec![
                (1000, "a"),
                (2500, "b"),
                (3250, "c"),
                (4125, "d"),
                (65500, "e")
            ]
        );
    }

    #[test]
    fn multi_timestamp_lines() {
        let l = parse_lrc("[00:12.00][01:30.50]Chorus\n[00:20.00]Verse");
        assert_eq!(
            synced(&l),
            vec![(12000, "Chorus"), (20000, "Verse"), (90500, "Chorus")]
        );
    }

    #[test]
    fn offset_positive_and_negative() {
        let l = parse_lrc("[offset:+500]\n[00:00.20]a\n[00:02.00]b");
        assert_eq!(synced(&l), vec![(0, "a"), (1500, "b")]);
        let l = parse_lrc("[00:02.00]b\n[offset:-250]");
        assert_eq!(synced(&l), vec![(2250, "b")]);
    }

    #[test]
    fn metadata_ignored() {
        let l =
            parse_lrc("[ar:Artist]\n[ti:Title]\n[al:Album]\n[by:me]\n[length:03:20]\n[00:01.00]x");
        assert_eq!(synced(&l), vec![(1000, "x")]);
    }

    #[test]
    fn malformed_skipped() {
        let l = parse_lrc("[00:01.00]ok\n[xx:yy]bad\n[00:02.zz]bad\n[00:03.00\n[00:04.00]ok2");
        assert_eq!(synced(&l), vec![(1000, "ok"), (4000, "ok2")]);
    }

    #[test]
    fn empty_lines_kept_as_gaps() {
        let l = parse_lrc("[00:01.00]a\n[00:05.00]\n[00:09.00]b");
        assert_eq!(synced(&l), vec![(1000, "a"), (5000, ""), (9000, "b")]);
    }

    #[test]
    fn enhanced_word_tags_stripped() {
        let l = parse_lrc("[00:01.00]<00:01.00>Hello <00:01.50>big <00:02.00>world <a>");
        assert_eq!(synced(&l), vec![(1000, "Hello big world <a>")]);
    }

    #[test]
    fn plain_fallback() {
        let l = parse_lrc("[ar:A]\n\nLine one\n\nLine two\n\n");
        assert_eq!(
            l,
            Lyrics::Plain(vec!["Line one".into(), "".into(), "Line two".into()])
        );
    }

    #[test]
    fn unsorted_sorted() {
        let l = parse_lrc("[00:03.00]c\n[00:01.00]a\n[00:02.00]b");
        assert_eq!(synced(&l), vec![(1000, "a"), (2000, "b"), (3000, "c")]);
    }

    #[test]
    fn bom_stripped() {
        let l = parse_lrc("\u{feff}[00:01.00]a");
        assert_eq!(synced(&l), vec![(1000, "a")]);
    }

    #[test]
    fn tags_and_length() {
        let text = "[ti:Song]\n[length: 03:20.50 ]\n[00:01.00]a";
        assert_eq!(tag_value(text, "TI"), Some("Song"));
        assert_eq!(tag_value(text, "ar"), None);
        assert_eq!(length_tag(text), Some(Duration::from_millis(200_500)));
        assert_eq!(length_tag("[length:03:20]"), Some(Duration::from_secs(200)));
        assert_eq!(length_tag("[length:soon]"), None);
        assert_eq!(length_tag("[00:01.00]a"), None);
        assert_eq!(format_timestamp(Duration::from_millis(62_340)), "01:02.34");
        assert_eq!(format_timestamp(Duration::from_millis(62_345)), "01:02.345");
    }

    #[test]
    fn decoding_fallbacks() {
        assert_eq!(decode_text("[00:01.00]é".as_bytes()), "[00:01.00]é");
        assert_eq!(decode_text(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode_text(b"caf\xE9"), "café");
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hé".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_text(&le), "hé");
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("hé".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode_text(&be), "hé");
    }

    #[test]
    fn to_lrc_round_trip() {
        let src = "[00:01.00]a\n[00:02.125]b\n[01:03.50]\n";
        let l = parse_lrc(src);
        assert_eq!(to_lrc(&l), src);
        assert_eq!(parse_lrc(&to_lrc(&l)), l);
        let p = Lyrics::Plain(vec!["x".into(), "y".into()]);
        assert_eq!(to_lrc(&p), "x\ny\n");
        assert_eq!(parse_lrc(&to_lrc(&p)), p);
    }
}
