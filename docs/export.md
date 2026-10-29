# Exporting lyric clips
Renders a share-ready MP4 of the Encore Classic, Classic or Vibrant layout (lyrics, album art, title and progress bar) for a chosen part of the current song. Rendering happens offline in the background on the GPU while the display keeps running; with the CPU encoding at the same time, a clip with the moving backdrop takes a few times its own length to export. Requires `ffmpeg` with libx264 and aac (see [Installation](../README.md#installation)).

## Formats
| Format | Resolution |
| --- | --- |
| 16:9 | 1920×1080 |
| 9:16 (default) | 1080×1920 |
| 1:1 | 1080×1080 |

Pick one or several with the **Size** chips; each becomes its own file. Portrait and square have their own layouts, which are also used when the live window has that shape. The Encore Classic style renders 16:9 only.

## The panel
Press `e` to open the export panel. It has two columns: **Clip** on the left for the range, and **Look** and **Output** on the right, with Cancel and Export below them. Narrow windows stack the columns.

## Look
The **Style** chips (**Classic**, **Vibrant** and **Encore Classic**) choose the look of the clip: the dark Classic layout over blurred art, the colourful Vibrant layout, or the Encore Classic layout (with the next song in the corner when the clip reaches the last 10% of the song). The panel starts on the style of the layout on screen. Everything else in the section starts at the display's current setting:
- **Lyrics** (**Left**, **Center**, **Right**) and **Flow** (**Classic**, **Stacked**): the Encore Classic style's lyric alignment and flow; disabled for the other styles.
- **Motion**: the moving backdrop, or a still one when off.
- **Artist background**: the artist image as the background; it starts at the live setting of the chosen style's layout and is disabled, with a note, when the [artists folder](artist-backgrounds.md) has no image for the song's artist.
- **Album colours**: [album colours](layouts.md#album-colours), taken from the artist image when it is the clip's background and from the cover otherwise, or the plain look when off.

Encore Classic clips also follow the display's skip-button setting (`s`).

## Choosing the range
The lines inside the clip are highlighted in the lyric list, and the range slider below it spans the whole song, with a tick at the start of every lyric line. All three controls stay in sync:
- Click the first lyric line, then the last (shift-click also works). The start snaps to 0.5 s before the first line, and the end to the start of the line after the last.
- Drag either handle of the range slider to move that end freely, or drag the highlighted part between them to move the whole range and keep its length. Pressing the bare track brings the nearer handle there. The handles stay at least 0.5 s apart and within the song.
- Nudge the start and end with − and +: 0.5 s per click, 5 s with Shift held.
- Alternatively, mark while listening: `[` sets the in-point and `]` the out-point, then press `e`.
- A clip can be at most 3 minutes long; dragging and nudging keep the range within that.
- Songs without synced lyrics show a note in place of the list; the slider and nudges still work.
- `Esc` closes the panel.

## Quality
The **Quality** chips, **High** and **Small**, pick the encoder settings; the panel remembers the choice until the display restarts and shows a rough size estimate of the files next to the chips. Notes below them say when the Encore Classic style limits the sizes to 16:9, when there is no artist image, when no audio file was found, and when the range cannot be exported.

| Preset | Video | Audio | Target |
| --- | --- | --- | --- |
| High quality (default) | H.264, CRF 19 | AAC 192 kbit/s | Close to the rendered frames, for keeping the clip or posting where the platform re-encodes anyway |
| Small file | H.264, CRF 24, peak bitrate capped at 3.8 Mbit/s for 1080×1920 (scaled by pixel count for other formats) | AAC 128 kbit/s | Messengers: a 30 s 9:16 clip stays under WhatsApp's 16 MB status limit |

Both presets use x264's `medium` preset with `-tune film` and auto-variance adaptive quantisation (`aq-mode=3`), which keeps dark, soft gradients from flattening into bands. Frames are converted to limited-range BT.709 YUV 4:2:0 with full chroma interpolation and tagged as BT.709, so colours match the display in players and browsers. Exported frames also carry a faint static grain (at most 2 of 255 levels, added over the blurred-art and gradient backdrops only, never over text, art or artist photos) that keeps the encoder from banding them; the live display has none.

## Audio
Audio is cut from the song's file (for MPD, in the MPD music directory; for an MPRIS player, the local file its track points to), with short fades, and can be toggled with the **Audio** switch. The music directory comes from `MPD_MUSIC_DIR` (see [Configuration](configuration.md#environment-variables)), else `music_directory` in `$XDG_CONFIG_HOME/mpd/mpd.conf` (`~/.config/mpd/mpd.conf` when unset), `~/.mpdconf`, or `/etc/mpd.conf`. If the file cannot be found, the clip is exported without audio and the toast says so. MPRIS tracks that are not local `file://` files (streams, for example) are exported without audio: "no audio: the player's track is not a local file".

## Output
Files are written to `~/Videos/encore/` (override with `ENCORE_EXPORT_DIR`) and named `<Artist> - <Title> (1m05-1m35) 9x16.mp4`, with ` (2)`, ` (3)`, ... appended if the name is taken. Exports run one at a time. A toast in the bottom-right corner shows progress, its × cancels the export, and clicking the "Saved" toast opens the folder.
