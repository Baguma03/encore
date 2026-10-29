# Lyrics
Lyrics are looked up in this order, and the first hit wins:
1. **Manual lyrics**: lyrics saved in the [lyrics manager](#lyrics-manager).
2. **Sidecar file**: `<song>.lrc`, or `.txt` for unsynced lyrics, next to the audio file. For MPD this requires `MPD_MUSIC_DIR` (see [Configuration](configuration.md)); for an MPRIS player it applies to local `file://` tracks, and streamed tracks skip this step.
3. **Local folder**: `./lyrics/<artist> - <title>.lrc` (or `.txt`). The primary artist and a bare `<title>` are also tried, and names are matched case-insensitively.
4. **Disk cache**: `~/.cache/encore/lyrics/`
5. **Online**: [LRCLIB](https://lrclib.net) (free, no API key needed), then KuGou. LRCLIB's exact match for the artist and title comes first, then the best synced match from LRCLIB's search, then the best synced match from KuGou's two top results. A synced match must name the same title and artist and be within 5 seconds of the song's length. Plain lyrics from LRCLIB are the last resort, and a song LRCLIB marks as instrumental gets no lyrics.

Online results are saved to the disk cache with a `[source:lrclib]` or `[source:kugou]` line naming the provider, so lyrics keep working offline afterwards. A song nothing is found for gets a `.notfound` marker in the cache and is not looked up online again for 7 days. When a lookup fails on a network error, the current song shows as loading and is retried after 2, 6 and 20 seconds, then no sooner than a minute later. Two lookups run at once: the most recently requested song goes first, and the next queued song is fetched ahead of time after the requested ones.

Synced lyrics use the standard LRC format (`[mm:ss.xx] line`), and the `[offset:+/-ms]` and `[length:mm:ss.xx]` tags are supported. Files without timestamps (and plain-text `.txt` files) are shown as unsynced lyrics. Lyrics files can be UTF-8, UTF-16 with a byte order mark, or Latin-1.

## Lyrics manager
`Shift`+`y` opens the lyrics manager for the current song, a panel in the same style and [album colours](layouts.md#album-colours) as the [export panel](export.md#the-panel); `Shift`+`y` again, `Esc`, the × in its corner or **Cancel** closes it without saving. While a text field has the keyboard no shortcut works, and `Esc` first leaves the field; in the sync view `Esc` goes back to the editor instead. If the song changes while it is open, it switches to the new song and clears the search.

Every view of the manager has a transport row above its buttons, for moving through the song while working on its lyrics: a play/pause button, buttons that seek 5 seconds back and forward (3 seconds in the sync view, like its arrow keys), and a seek bar between the playback position (`m:ss.xx`) and the song's length. Clicking the bar seeks there; dragging it shows the position under the pointer and seeks on release. Seeks work while paused, stop 1 second short of the end (reaching the end would finish the song and move on to the next one), and the position, the editor's readout and the lyrics behind the panel follow at once. The controls work while the song the manager is open for is the one playing. Below the row, key caps list its keys:

| Key | Action |
| --- | --- |
| `←` / `→` | Seek 5 seconds back / forward, while no text field has the keyboard |
| `p` | Play/pause, while no text field has the keyboard |
| `Alt`+`←` / `Alt`+`→` | Seek 5 seconds back / forward, also while typing in the search field or the editor |
| `Alt`+`p` | Play/pause, also while typing |

The manager takes these keys for itself, so the text fields neither move the cursor nor type a character for them, and the app's seeking and play/pause shortcuts don't act while it is open.

- **Current lyrics**: the card at the top shows the lyrics on the display and where they came from, such as "Synced (Cache (LRCLIB))" or "Plain (Manual)", with the timing offset when one is set. Its buttons copy the lyrics to the clipboard, open the timing view, and, for saved manual lyrics, remove them after a confirmation, which looks the song up again.
- **Search**: the field searches LRCLIB and KuGou for its text; left empty it searches for the song's own title and artist. The arrow beside it fills in "Artist - Title". `Enter` or **Search** starts a search. Results are ranked by how well their title and artist match (title words weigh more than the artist, and instrumental or karaoke versions rank lower), then synced before plain, then by the closest length, and identical lyrics are listed once. Each result shows its provider, whether it is synced, its title and artist, its length with the difference from the song's, and whether it is a sure, good or weak match, with its first lines. Providers that fail are noted above the results. Searching needs online lyrics (`lyrics_online`).
- **Add LRC file**: picks a `.lrc` or `.txt` file in the system's file dialog, through the XDG Desktop Portal, with `zenity` or `kdialog` as a fallback when no portal answers. Files can also be dropped anywhere on the window while the manager is open. The file is listed as "File: <name>".
- **Editor**: clicking the current lyrics, a result or the imported file puts its text in the editor, where it can be changed; **Reset** discards the changes. The line under it counts the lyric lines and warns when there are none. Beside it, the playback position reads to the hundredth of a second, and **Insert time** puts it at the editor's cursor as `[mm:ss.xx]`; at the start of a line that already begins with a timestamp it replaces that timestamp, and without a cursor it goes at the start of the first lyric line without one. **Sync** opens the [sync view](#sync-view) on the editor's text.
- **Save** stores the editor's text as the song's manual lyrics, which the display shows at once and which take precedence over every other source. They are saved as `~/.cache/encore/lyrics/manual/<artist> - <title>.lrc` when they have timestamps, else `.txt`.

The timing view moves the panel to the side of the window away from the lyrics and dims the layout less, so the lyrics stay readable behind it. It shows the previous, active and next lyric lines at the playback position with their times, and changes apply to the display at once:
- **Offset**: −0.5 s, −0.1 s, +0.1 s and +0.5 s nudges, up to a minute either way, with **Reset**. Positive values show every line later, negative earlier; the offset adds to the file's own `[offset:]` tag.
- **Stretch to song length**: scales every line time by the song's length divided by the lyrics' `[length:]` tag, for sped-up or slowed versions of the recording the lyrics were timed against. It needs synced lyrics with a `[length:]` tag within a factor of four of the song's length.

The offset and stretch are kept per song in `~/.cache/encore/lyrics/manual/<artist> - <title>.json`, whichever source the lyrics come from, and apply to the display and to exported clips.

### Sync view
The sync view times the editor's lyrics line by line while the song plays. It works on synced LRC and on plain lyrics alike, docks to the side like the timing view so the lyrics stay visible behind it, and shows the playback position large, to the hundredth of a second, above a list of the lines with their times (`--:--.--` for a line without one). The line the next stamp goes to is outlined, and the line showing at the playback position has an accent bar and the accent colour. As lines get times the display behind the panel shows them as they would look once saved; nothing is saved until **Save** in the editor.

| Key | Action |
| --- | --- |
| `Space` or `Enter` | Stamp the outlined line with the playback position and move to the next line |
| `Backspace` | Step back to the line before and clear its time, to stamp it again |
| `←` / `→` | Seek 3 seconds back / forward (also the transport's **−3 s** and **+3 s**) |
| `p` | Play/pause (also the transport's play/pause button) |
| `g` | Insert an instrumental break (a blank timestamped line, shown as `• • •`) before the next line, stamped now (also the **Gap** button) |
| `Esc` | Back to the editor without the times (also **Cancel**) |

While the view is open these keys belong to it and trigger no other shortcut; `Shift`+`y` and `F11` still work. A key stamp records the position 0.12 seconds early (`REACTION` in `src/lyrics/sync_edit.rs`) to make up for the delay between hearing a line start and pressing the key; while the song is paused the position is used as it is. Stamps account for the file's `[offset:]` tag and the song's timing offset and stretch, so a line shows at the position it was stamped at.

Clicking a line makes it the next line to stamp. Blank lines between stanzas are stepped over when stamping; clicking one and stamping it turns it into an instrumental break. Each line's own controls, on the outlined and the hovered line, also work without tapping along:
- Clicking a line's time plays from 1 second before the line, to hear the lead-in.
- **−** and **+** move that line 0.1 seconds earlier or later.
- The stopwatch sets the line to the playback position.

A time earlier than the line before it is shown in the warning colour and counted under the list; lines are never reordered. **Clear** removes every time. **Apply** writes the lines back into the editor as LRC: the `[ar:]`, `[al:]`, `[ti:]`, `[offset:]` and other tags stay, a `[length:]` tag with the song's length is added when there is none, lines without a time keep their text without a timestamp (the display leaves them out), and blank lines without a time are dropped.
