//! Line-by-line timing of lyrics text: the model behind the lyrics manager's sync view.
//!
//! Editor text (synced LRC or plain lyrics) becomes a [`SyncDoc`] of lines with optional
//! times. Times in a document are file times, as written in the LRC; a [`Clock`] maps them to
//! song positions through the file's `[offset:]` tag and the song's timing adjustments, so a
//! line stamped at a song position shows at that position once saved.

use std::time::Duration;

use super::lrc::{clean_text, format_timestamp, is_metadata, parse_timestamp, tag_value};
use super::timing::{Adjustments, Timing};
use super::{LyricLine, Lyrics};

/// Subtracted from the playback position when a line is stamped while the song plays: the
/// usual delay between hearing a line start and pressing the key.
pub const REACTION: Duration = Duration::from_millis(120);

/// Stamps are rounded to this many milliseconds, the precision of `mm:ss.xx`.
const STAMP_STEP: u64 = 10;

/// A lyric line and its time in the file, if it has one. Empty text is an instrumental gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncLine {
    pub time: Option<Duration>,
    pub text: String,
}

impl SyncLine {
    pub fn is_gap(&self) -> bool {
        self.text.is_empty()
    }

    /// A blank line without a time: a stanza break, which stamping steps over.
    fn is_break(&self) -> bool {
        self.time.is_none() && self.is_gap()
    }
}

/// Lyrics text as editable lines, with its metadata tags kept for writing it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncDoc {
    /// Metadata tag lines (`[ar:...]`, `[offset:...]`, ...) as written, in order.
    tags: Vec<String>,
    pub lines: Vec<SyncLine>,
    /// The `[offset:]` tag in milliseconds.
    offset_ms: i64,
    /// The `[length:]` tag.
    length: Option<Duration>,
}

impl SyncDoc {
    /// Splits LRC or plain text into lines. A line with several timestamps becomes one line
    /// per timestamp. Lines that the display would skip (malformed tags) are dropped, and
    /// blank lines are trimmed from both ends and collapsed. In text with timestamps, blank
    /// lines without one are dropped, and when every line has a time the lines are put in
    /// time order, as the display shows them.
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut tags = Vec::new();
        let mut lines: Vec<SyncLine> = Vec::new();
        for raw in text.lines() {
            match parse_line(raw) {
                Parsed::Tag => tags.push(raw.trim().to_owned()),
                Parsed::Skip => {}
                Parsed::Line(stamps, text) if stamps.is_empty() => {
                    let blank = text.is_empty();
                    let after_blank = lines.last().is_none_or(SyncLine::is_break);
                    if !(blank && after_blank) {
                        lines.push(SyncLine { time: None, text });
                    }
                }
                Parsed::Line(stamps, text) => {
                    lines.extend(stamps.into_iter().map(|ms| SyncLine {
                        time: Some(Duration::from_millis(ms)),
                        text: text.clone(),
                    }));
                }
            }
        }
        while lines.last().is_some_and(SyncLine::is_break) {
            lines.pop();
        }
        if lines.iter().any(|l| l.time.is_some()) {
            lines.retain(|l| !l.is_break());
            if lines.iter().all(|l| l.time.is_some()) {
                lines.sort_by_key(|l| l.time);
            }
        }
        let joined = tags.join("\n");
        Self {
            offset_ms: tag_value(&joined, "offset")
                .and_then(|v| v.trim_start_matches('+').parse().ok())
                .unwrap_or(0),
            length: super::lrc::length_tag(&joined),
            tags,
            lines,
        }
    }

    /// The document as LRC: its tags, a `[length:]` tag from `duration` when it has none and
    /// any line is stamped, then its lines. Lines without a time keep their text without a
    /// timestamp; blank ones are left out once any line is stamped. Without any stamp the
    /// text stays plain lyrics.
    pub fn to_text(&self, duration: Option<Duration>) -> String {
        let synced = self.stamped() > 0;
        let mut out = String::new();
        for tag in &self.tags {
            out.push_str(tag);
            out.push('\n');
        }
        if let Some(duration) = duration.filter(|d| synced && self.length.is_none() && !d.is_zero())
        {
            out.push_str(&format!("[length:{}]\n", format_timestamp(duration)));
        }
        for line in &self.lines {
            match line.time {
                Some(time) => {
                    out.push('[');
                    out.push_str(&format_timestamp(time));
                    out.push(']');
                    out.push_str(&line.text);
                }
                None if synced && line.is_gap() => continue,
                None => out.push_str(&line.text),
            }
            out.push('\n');
        }
        out
    }

    /// The clock for this document's tags, the song's adjustments and its duration.
    pub fn clock(&self, adj: Adjustments, duration: Option<Duration>) -> Clock {
        Clock {
            offset_ms: self.offset_ms,
            timing: Timing::new(adj, self.length, duration),
        }
    }

    pub fn stamped(&self) -> usize {
        self.lines.iter().filter(|l| l.time.is_some()).count()
    }

    /// Lines with words that have no time yet.
    pub fn unstamped(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.time.is_none() && !l.is_gap())
            .count()
    }

    /// The first line with words and no time, where stamping starts.
    pub fn first_open(&self) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| l.time.is_none() && !l.is_gap())
    }

    /// The line stamped after line `i`: the next one that is not a stanza break.
    pub fn next_after(&self, i: usize) -> Option<usize> {
        (i + 1..self.lines.len()).find(|&j| !self.lines[j].is_break())
    }

    /// The line before line `i` that is not a stanza break.
    pub fn prev_before(&self, i: usize) -> Option<usize> {
        (0..i.min(self.lines.len()))
            .rev()
            .find(|&j| !self.lines[j].is_break())
    }

    /// Sets or clears line `i`'s time.
    pub fn set(&mut self, i: usize, time: Option<Duration>) {
        if let Some(line) = self.lines.get_mut(i) {
            line.time = time.map(round_stamp);
        }
    }

    /// Moves line `i` by `ms` milliseconds, not before zero. Lines without a time stay so.
    pub fn nudge(&mut self, i: usize, ms: i64) {
        if let Some(line) = self.lines.get_mut(i) {
            if let Some(time) = line.time {
                let moved = (time.as_millis() as i64 + ms).max(0) as u64;
                line.time = Some(round_stamp(Duration::from_millis(moved)));
            }
        }
    }

    /// Inserts an instrumental gap stamped `time` before line `i`, or at the end.
    pub fn insert_gap(&mut self, i: usize, time: Duration) {
        let line = SyncLine {
            time: Some(round_stamp(time)),
            text: String::new(),
        };
        self.lines.insert(i.min(self.lines.len()), line);
    }

    /// Clears every line's time.
    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.time = None;
        }
    }

    /// For each line, whether it is stamped earlier than the stamped line before it.
    pub fn out_of_order(&self) -> Vec<bool> {
        let mut last: Option<Duration> = None;
        self.lines
            .iter()
            .map(|l| match l.time {
                Some(time) => {
                    let early = last.is_some_and(|prev| time < prev);
                    last = Some(last.map_or(time, |prev| prev.max(time)));
                    early
                }
                None => false,
            })
            .collect()
    }
}

/// Maps file times in a document to song positions and back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    offset_ms: i64,
    timing: Timing,
}

impl Clock {
    pub const IDENTITY: Clock = Clock {
        offset_ms: 0,
        timing: Timing::IDENTITY,
    };

    /// Song position at which a line stamped `file` shows.
    pub fn song_time(&self, file: Duration) -> Duration {
        let ms = (file.as_millis() as i64 - self.offset_ms).max(0) as u64;
        self.timing.song_time(Duration::from_millis(ms))
    }

    /// The file time that shows at song position `song`, rounded to a stamp.
    pub fn file_time(&self, song: Duration) -> Duration {
        let ms = (self.timing.lyric_time(song).as_millis() as i64 + self.offset_ms).max(0);
        round_stamp(Duration::from_millis(ms as u64))
    }

    /// The stamped lines at their song positions, in time order, as the display would show
    /// them once saved.
    pub fn lyrics(&self, doc: &SyncDoc) -> Lyrics {
        let mut lines: Vec<LyricLine> = doc
            .lines
            .iter()
            .filter_map(|l| {
                Some(LyricLine {
                    time: self.song_time(l.time?),
                    text: l.text.clone(),
                })
            })
            .collect();
        lines.sort_by_key(|l| l.time);
        Lyrics::Synced(lines)
    }

    /// The line showing at song position `elapsed`: the stamped line with the latest time
    /// not after it, the later line on a tie.
    pub fn active(&self, doc: &SyncDoc, elapsed: Duration) -> Option<usize> {
        let mut best: Option<(usize, Duration)> = None;
        for (i, line) in doc.lines.iter().enumerate() {
            let Some(time) = line.time.map(|t| self.song_time(t)) else {
                continue;
            };
            if time <= elapsed && best.is_none_or(|(_, b)| time >= b) {
                best = Some((i, time));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The first stamped line that shows after song position `elapsed`.
    pub fn upcoming(&self, doc: &SyncDoc, elapsed: Duration) -> Option<usize> {
        doc.lines
            .iter()
            .position(|l| l.time.is_some_and(|t| self.song_time(t) > elapsed))
    }
}

/// A tap-to-sync pass over a document: the line the next tap stamps, and the taps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSession {
    pub doc: SyncDoc,
    /// The line the next stamp goes to; None once the last line is stamped.
    pub next: Option<usize>,
}

impl SyncSession {
    /// Starts at the first line without a time, or at the first line when all have one.
    pub fn new(doc: SyncDoc) -> Self {
        let next = doc
            .first_open()
            .or_else(|| (!doc.lines.is_empty()).then_some(0));
        Self { doc, next }
    }

    /// Stamps the next line with `time` and moves on to the line after it, stepping over
    /// stanza breaks.
    pub fn stamp(&mut self, time: Duration) {
        if let Some(i) = self.next {
            self.doc.set(i, Some(time));
            self.next = self.doc.next_after(i);
        }
    }

    /// Steps back to the line before the next one and clears its time, to stamp it again.
    pub fn back(&mut self) {
        let from = self.next.unwrap_or(self.doc.lines.len());
        if let Some(j) = self.doc.prev_before(from) {
            self.doc.set(j, None);
            self.next = Some(j);
        }
    }

    /// Inserts an instrumental gap stamped `time` before the next line; the next line stays
    /// the same.
    pub fn gap(&mut self, time: Duration) {
        let at = self.next.unwrap_or(self.doc.lines.len());
        self.doc.insert_gap(at, time);
        self.next = self.next.map(|i| i + 1);
    }

    pub fn select(&mut self, i: usize) {
        if i < self.doc.lines.len() {
            self.next = Some(i);
        }
    }
}

/// The time a stamp records at playback position `position`: [`REACTION`] earlier while the
/// song plays, the position itself while it is paused.
pub fn tap_time(position: Duration, playing: bool) -> Duration {
    if playing {
        position.saturating_sub(REACTION)
    } else {
        position
    }
}

/// Puts the timestamp `[stamp]` into `text` at character index `cursor`. At the start of a
/// line that already begins with a timestamp it replaces that timestamp. Without a cursor it
/// goes at the start of the first lyric line without a timestamp, or on a new last line.
/// Returns the new text and the character index just after the timestamp.
pub fn insert_stamp(text: &str, cursor: Option<usize>, stamp: Duration) -> (String, usize) {
    let tag = format!("[{}]", format_timestamp(stamp));
    let tag_chars = tag.chars().count();
    let at = match cursor {
        Some(c) => text.char_indices().nth(c).map_or(text.len(), |(b, _)| b),
        None => {
            let mut start = 0;
            let mut found = None;
            for line in text.split_inclusive('\n') {
                let trimmed = line.trim();
                let open = !trimmed.is_empty()
                    && matches!(parse_line(line), Parsed::Line(ref s, _) if s.is_empty());
                if open {
                    found = Some(start + (line.len() - line.trim_start().len()));
                    break;
                }
                start += line.len();
            }
            match found {
                Some(at) => at,
                None => {
                    let mut out = text.to_owned();
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    let index = out.chars().count() + tag_chars;
                    out.push_str(&tag);
                    return (out, index);
                }
            }
        }
    };
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut end = at;
    if at == line_start {
        let rest = &text[at..];
        if let Some(close) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
            if parse_timestamp(&rest[1..=close]).is_some() {
                end = at + close + 2;
            }
        }
    }
    let out = format!("{}{tag}{}", &text[..at], &text[end..]);
    (out, text[..at].chars().count() + tag_chars)
}

fn round_stamp(time: Duration) -> Duration {
    let ms = time.as_millis() as u64;
    Duration::from_millis((ms + STAMP_STEP / 2) / STAMP_STEP * STAMP_STEP)
}

enum Parsed {
    /// A metadata tag line.
    Tag,
    /// A line the display skips: a malformed tag.
    Skip,
    /// Timestamps (possibly none) and the cleaned text.
    Line(Vec<u64>, String),
}

/// One line read the way [`super::lrc::parse_lrc`] reads it.
fn parse_line(raw: &str) -> Parsed {
    let mut rest = raw.trim();
    let mut stamps = Vec::new();
    while let Some(after) = rest.strip_prefix('[') {
        let Some(end) = after.find(']') else {
            return Parsed::Skip;
        };
        let tag = &after[..end];
        if let Some(ms) = parse_timestamp(tag) {
            stamps.push(ms);
        } else if stamps.is_empty() && is_metadata(tag) {
            return Parsed::Tag;
        } else {
            return Parsed::Skip;
        }
        rest = after[end + 1..].trim_start();
    }
    Parsed::Line(stamps, clean_text(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::lrc::parse_lrc;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn times(doc: &SyncDoc) -> Vec<Option<u64>> {
        doc.lines
            .iter()
            .map(|l| l.time.map(|t| t.as_millis() as u64))
            .collect()
    }

    fn texts(doc: &SyncDoc) -> Vec<&str> {
        doc.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn plain_lyrics_become_open_lines() {
        let doc = SyncDoc::parse("\n\nOne\n\n\n Two  words \n[Chorus]\nThree\n\n");
        assert_eq!(texts(&doc), vec!["One", "", "Two words", "Three"]);
        assert_eq!(times(&doc), vec![None; 4]);
        assert_eq!(doc.unstamped(), 3);
        assert_eq!(doc.first_open(), Some(0));
        // Nothing stamped: the text stays plain, stanza break included.
        assert_eq!(doc.to_text(Some(ms(200_000))), "One\n\nTwo words\nThree\n");
    }

    #[test]
    fn synced_lyrics_keep_tags_and_order() {
        let text = "[ar:A]\n[ti:T]\n[00:12.00][00:30.00]Chorus\n\n[00:20.00]Verse\n[00:25.00]\n";
        let doc = SyncDoc::parse(text);
        assert_eq!(texts(&doc), vec!["Chorus", "Verse", "", "Chorus"]);
        assert_eq!(
            times(&doc),
            vec![Some(12_000), Some(20_000), Some(25_000), Some(30_000)]
        );
        let out = doc.to_text(Some(ms(203_000)));
        assert_eq!(
            out,
            "[ar:A]\n[ti:T]\n[length:03:23.00]\n[00:12.00]Chorus\n[00:20.00]Verse\n\
             [00:25.00]\n[00:30.00]Chorus\n"
        );
        assert_eq!(parse_lrc(&out), parse_lrc(text));
        // An existing length tag is kept, not added twice.
        let doc = SyncDoc::parse("[length:03:00]\n[00:01.00]a");
        assert_eq!(doc.to_text(Some(ms(5000))), "[length:03:00]\n[00:01.00]a\n");
    }

    #[test]
    fn partly_stamped_text_keeps_its_order() {
        let doc = SyncDoc::parse("[00:05.00]b\nnew line\n\n[00:01.00]a");
        assert_eq!(texts(&doc), vec!["b", "new line", "a"]);
        assert_eq!(doc.first_open(), Some(1));
        assert_eq!(doc.out_of_order(), vec![false, false, true]);
        assert_eq!(doc.to_text(None), "[00:05.00]b\nnew line\n[00:01.00]a\n");
    }

    #[test]
    fn tapping_through_plain_lyrics() {
        let mut s = SyncSession::new(SyncDoc::parse("One\nTwo\n\nThree"));
        assert_eq!(s.next, Some(0));
        s.stamp(ms(1004));
        s.stamp(ms(2500));
        // The stanza break is stepped over.
        assert_eq!(s.next, Some(3));
        s.stamp(ms(4000));
        assert_eq!(s.next, None);
        s.stamp(ms(9000));
        assert_eq!(
            times(&s.doc),
            vec![Some(1000), Some(2500), None, Some(4000)]
        );
        assert_eq!(
            s.doc.to_text(Some(ms(60_000))),
            "[length:01:00.00]\n[00:01.00]One\n[00:02.50]Two\n[00:04.00]Three\n"
        );
        // Back clears the last stamp and makes it the next line again.
        s.back();
        assert_eq!(s.next, Some(3));
        assert_eq!(s.doc.lines[3].time, None);
        s.back();
        assert_eq!(s.next, Some(1));
        assert_eq!(times(&s.doc), vec![Some(1000), None, None, None]);
        s.back();
        s.back();
        assert_eq!(s.next, Some(0));
        assert_eq!(s.doc.stamped(), 0);
    }

    #[test]
    fn gaps_and_selection() {
        let mut s = SyncSession::new(SyncDoc::parse("One\nTwo"));
        s.stamp(ms(1000));
        s.gap(ms(3000));
        assert_eq!(texts(&s.doc), vec!["One", "", "Two"]);
        assert_eq!(s.next, Some(2));
        s.stamp(ms(5000));
        s.gap(ms(9000));
        assert_eq!(texts(&s.doc), vec!["One", "", "Two", ""]);
        assert_eq!(s.next, None);
        // A stamped blank line is a gap like any other line.
        s.select(1);
        s.stamp(ms(3500));
        assert_eq!(s.next, Some(2));
        assert_eq!(times(&s.doc)[1], Some(3500));
        s.select(10);
        assert_eq!(s.next, Some(2));
        // A fully stamped document starts at its first line.
        let full = SyncSession::new(SyncDoc::parse("[00:01.00]a\n[00:02.00]b"));
        assert_eq!(full.next, Some(0));
        assert_eq!(SyncSession::new(SyncDoc::parse("")).next, None);
    }

    #[test]
    fn nudges_round_and_clamp() {
        let mut doc = SyncDoc::parse("[00:01.00]a\nb");
        doc.nudge(0, -100);
        assert_eq!(doc.lines[0].time, Some(ms(900)));
        doc.nudge(0, -5000);
        assert_eq!(doc.lines[0].time, Some(ms(0)));
        doc.nudge(1, 100);
        assert_eq!(doc.lines[1].time, None);
        doc.set(1, Some(ms(12_346)));
        assert_eq!(doc.lines[1].time, Some(ms(12_350)));
        doc.clear();
        assert_eq!(doc.stamped(), 0);
    }

    #[test]
    fn out_of_order_compares_with_the_latest_earlier_stamp() {
        let doc = SyncDoc::parse("[00:05.00]a\nb\n[00:03.00]c\n[00:04.00]d\n[00:06.00]e");
        assert_eq!(doc.out_of_order(), vec![false, false, true, true, false]);
    }

    #[test]
    fn clock_follows_offset_tag_and_adjustments() {
        let doc = SyncDoc::parse("[offset:+500]\n[length:00:10.00]\n[00:02.00]a\n[00:04.00]b");
        let adj = Adjustments {
            offset_ms: 1000,
            stretch: true,
        };
        let clock = doc.clock(adj, Some(ms(20_000)));
        // (2000 - 500) * 2 + 1000
        assert_eq!(clock.song_time(ms(2000)), ms(4000));
        assert_eq!(clock.file_time(ms(4000)), ms(2000));
        assert_eq!(clock.file_time(ms(4013)), ms(2010));
        assert_eq!(clock.lyrics(&doc), {
            let t = Timing::new(adj, Some(ms(10_000)), Some(ms(20_000)));
            t.apply(&parse_lrc(&doc.to_text(None)))
        });
        assert_eq!(clock.active(&doc, ms(3999)), None);
        assert_eq!(clock.active(&doc, ms(5000)), Some(0));
        assert_eq!(clock.active(&doc, ms(9000)), Some(1));
        assert_eq!(clock.upcoming(&doc, ms(5000)), Some(1));
        assert_eq!(clock.upcoming(&doc, ms(9000)), None);
        assert_eq!(Clock::IDENTITY.file_time(ms(1234)), ms(1230));
    }

    #[test]
    fn preview_skips_open_lines_and_sorts() {
        let doc = SyncDoc::parse("[00:05.00]b\nopen\n[00:01.00]a");
        let Lyrics::Synced(lines) = Clock::IDENTITY.lyrics(&doc) else {
            panic!("expected synced");
        };
        let got: Vec<(u64, &str)> = lines
            .iter()
            .map(|l| (l.time.as_millis() as u64, l.text.as_str()))
            .collect();
        assert_eq!(got, vec![(1000, "a"), (5000, "b")]);
        // The tie goes to the later line.
        let doc = SyncDoc::parse("[00:01.00]a\n[00:01.00]b");
        assert_eq!(Clock::IDENTITY.active(&doc, ms(1000)), Some(1));
    }

    #[test]
    fn reaction_applies_while_playing() {
        assert_eq!(tap_time(ms(10_000), true), ms(10_000) - REACTION);
        assert_eq!(tap_time(ms(10_000), false), ms(10_000));
        assert_eq!(tap_time(ms(50), true), ms(0));
    }

    #[test]
    fn inserting_timestamps() {
        let t = ms(62_340);
        // At the cursor.
        assert_eq!(
            insert_stamp("ab\ncd", Some(3), t),
            ("ab\n[01:02.34]cd".into(), 13)
        );
        assert_eq!(insert_stamp("ab", Some(1), t), ("a[01:02.34]b".into(), 11));
        // A timestamp at the start of the cursor's line is replaced.
        assert_eq!(
            insert_stamp("x\n[00:01.00]cd", Some(2), t),
            ("x\n[01:02.34]cd".into(), 12)
        );
        // Characters, not bytes.
        assert_eq!(
            insert_stamp("é\nü", Some(2), t),
            ("é\n[01:02.34]ü".into(), 12)
        );
        // Without a cursor: the first lyric line without a timestamp.
        assert_eq!(
            insert_stamp("[ar:x]\n[00:01.00]a\n\n  b\nc", None, t),
            ("[ar:x]\n[00:01.00]a\n\n  [01:02.34]b\nc".into(), 32)
        );
        assert_eq!(
            insert_stamp("[00:01.00]a", None, t),
            ("[00:01.00]a\n[01:02.34]".into(), 22)
        );
        assert_eq!(insert_stamp("", None, t), ("[01:02.34]".into(), 10));
        // A cursor past the end inserts at the end.
        assert_eq!(insert_stamp("a", Some(9), t), ("a[01:02.34]".into(), 11));
    }
}
