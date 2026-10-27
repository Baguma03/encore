# Encore: A Spotify/Apple Music-like graphical display for MPD and MPRIS players

Encore displays whatever is playing on an [MPD](https://www.musicpd.org) server or on any MPRIS player (Spotify, Firefox, VLC, mpv, Rhythmbox...). Note that this is not a full player or MPD client, as it only *displays* data (plus basic playback controls).

This is a replacement for my old [mpd-display](https://github.com/allylikesu/mpd-display), which was coded in Python using the Pygame engine. It was coded in one night, and I am not very proud of it.

### Improvements from python version
- Faster start-up
- Much lighter-weight, using Rust with [eframe/egui](https://crates.io/crates/eframe)
- Runs natively on both Wayland and X11
- Smooth updating progress bar
- Doesn't spam mpc commands every frame (Uses the [mpd](https://crates.io/crates/mpd) crate for updating status, gets album art on song change using `mpc readpicture`)
- Reconnects automatically if MPD goes away or restarts
- Follows MPD or any MPRIS player, [switching between them](#players) as they come and go
- Time-synced lyrics
- Many more. The amount of sins that were committed in that old version still keep me up at night.

Only tested on Linux. Needs either an MPD server (and `mpc`, used to fetch its album art) or any MPRIS-capable player; see [Prerequisites](#prerequisites). `ffmpeg` (with libx264 and aac) is optional and only needed for [exporting lyric clips](#exporting-lyric-clips).

## Layouts
Three layouts are available. `l` cycles through them at runtime (Apple → Vibrant → Spotify), and `ENCORE_LAYOUT` chooses the starting one.

- **Apple Music-style** (default): a blurred album art backdrop, rounded album art with the title, "Artist — Album" and a progress bar below it, clickable previous/play-pause/next buttons, a seekable progress bar, and a time-synced lyrics column on the right. The active lyric line is highlighted, the column scrolls smoothly, and lines far from the active one fade out. An optional date/clock overlay can be toggled with `t`.
- **Vibrant**: modelled on Apple Music's colourful lyrics view. The backdrop is bright: enlarged, heavily blurred copies of the album art with a faint gradient of its main colours over them, cross-fading between songs. Light covers stay light; a soft dark halo behind the text, and a mild wash on very light art, keep white text readable. Square album art sits on the left with the elapsed and remaining time, a thin seekable progress bar, and the title and "Artist — Album" centred beneath it, and the time-synced lyrics are on the right. There are no transport buttons; clicking anywhere toggles play/pause. Tall and square windows stack the art above the lyrics. The `t` clock overlay works here too.
- **Spotify-style**: modelled on the Spotify desktop client's fullscreen mode, with the artist's background image (see [below](#obtaining-and-using-artist-background-images-from-spotify)). The album art, with rounded corners and a soft shadow, sits on the left with the large title and artist beside it. A rounded-square play/pause button, styled after the Quickshell (illogical-impulse) media widget, sits at the far right with its bottom level with the art's. Along the bottom of the screen runs a thin seekable wavy progress bar with previous and next buttons at its ends, the previous button level with the art's left edge and the next button with the play button's right edge; `s` hides the buttons, and the bar then runs from the art's left edge to the play button's right edge. The elapsed part of the bar is a sine wave that moves while playing and flattens when paused, followed by a vertical bar handle and the flat remaining track; click or drag it to seek. The icons are the filled, rounded Material Symbols, drawn as vector shapes. Clicking anywhere else toggles play/pause. Text, icons, the bar and the button take their colours from the cover (see [Album colours](#album-colours)). The lyrics sit above the title in bold at the same size as in the Vibrant layout, without a shadow; lines differ only in brightness. The top of the active line stays at a fixed height, 60% of the art's height above the art, and a wrapped active line grows downwards. The lines run in a column as wide as the Vibrant layout's lyrics column and wrap where they do there, each line at the column's width scaled by its size, so a lyric breaks at the same words in both layouts; nothing comes closer to the title than one and a half times the gap between the art and the title. Text reaching that edge, or the Up next card's, dissolves into it: the glyphs fade out gradually over about one line's height instead of being cut off. `j` cycles the column's alignment: right-aligned with its right edge on the play button's right edge (the default), left-aligned with its left edge on the title's left edge, centred between the two, or the right-hand column with its lines left-aligned. `o` cycles through three flows for time-synced lyrics:
  - **Stacked** (default), after Apple Music: the active line sits at the top of the stack, brightest, with the next three lines below it, each fainter than the one above. On each new line the stack slides up, the outgoing line fading out as it leaves and the next one brightening into the top slot, with a slight vertical smear while the lines move.
  - **Classic**: four lines: the previous one faint above the active one, the next one faint below it, and the one after that barely visible beneath. On each new line they scroll up smoothly, the outgoing line fading out at the top, the barely visible line brightening into the next line's place and a new one fading in at the bottom.
  - **Scrolling**, like the Apple and Vibrant layouts' lyrics: the whole lyrics scroll past, the active line at its fixed height, larger and brightest, with the lines before and after it smaller and dimmer the further they are from it. On each new line the list scrolls up and the lines grow into or out of the active style.

  `• • •` stands for the time before the first line and for instrumental gaps. Unsynced lyrics use the classic flow and advance through the song at an even pace. While the Up next card (see below) shows, lines above the active one that reach under it dissolve; the active line stays where it is. The `t` date and clock, in the same format as the other layouts, sit centred above the album art.
- **Up next**: in every layout, for the last 10% of a song with another one queued, the next song slides into the top-right corner: its rounded cover with its title and artist beside it, and a thin bar below counting down the rest of the current song, drawn straight over the backdrop in the layout's own text colours (with the Vibrant layout's dark halo behind the text). In the Apple and Vibrant layouts, lyric lines reaching under the card fade out while the active line stays put, and tall and square windows move their content down below the card while it shows.
- **Moving backdrop**: enlarged, blurred copies of the album art drift across the screen, turn, swell and blend into each other, like Apple Music's animated artwork. It keeps moving while playback is paused. Toggle it with `b`, or start without it with `ENCORE_MOTION=off`; with it off, the backdrop is still.
- **Backgrounds**: `a` toggles the artist background for the current layout. When it is on and `artists/` has an image for the song's artist, the layout uses that image: tinted dark in the Spotify and Apple layouts with a vignette in the Apple one, and lighter in the Vibrant layout, with a halo behind the text. With motion on, the image slowly pans and zooms. When it is off, or there is no image, each layout uses its own backdrop: blurred album art for Spotify (moving when motion is on, darkened to match), dark blurred art for Apple and the colourful art for Vibrant. The setting is kept per layout for the session; it starts on for Spotify and off for Apple and Vibrant, and `ENCORE_ARTIST_BG` sets the starting value for all of them.

### Album colours
Every layout takes its colours from the album art. The cover's average colour is blended into a small Material 3 dark scheme (primary, on-primary, secondary container and text colours), the same way the Quickshell media widget adapts its colours. The main text is a light pastel of the cover's hue, still the brightest text on screen; grey covers give near-white text.

- **Spotify**: the title, active lyric line, clock and the next song's title use the pastel, the date uses the scheme's muted text colour, and the other lyric lines are fainter versions of it; the artist uses the scheme's muted text colour; the progress bar uses the primary colour for the elapsed part and handle and the secondary container colour for the rest of the track; and the play/pause button is filled with the primary colour while playing and the secondary container colour while paused.
- **Apple**: the title, active lyric line, filled part of the progress bar and all buttons use the pastel; "Artist — Album", the other lyric lines and the times use a slightly deeper shade of it, at their usual opacity. The Up next card takes the title's, subtitle's and progress bar's colours.
- **Vibrant**: as in the Apple layout, but the pastel is mixed towards white until the text keeps at least 80% of white text's contrast against the bright backdrop (70% for the fainter lines).
- **Panels**: the [keyboard shortcuts overlay](#keyboard-shortcuts-overlay), the [clip export panel](#the-panel) and the [lyrics manager](#lyrics-manager) share one palette. The card is a dark surface with a faint hint of the cover's hue, and the dimmed backdrop behind it takes the same hint; titles use the pastel, and descriptions, key caps, outlines and hints are quieter shades of it. The overlay's section headings, selected chips, switches that are on, the Export button, the range slider's fill and handles, and the highlighted lyric lines use the primary colour, with dark text on filled controls. Tooltips and scroll bars inside the export panel follow the same palette.

Every colour keeps a minimum contrast against its backdrop, so light, dark and very saturated covers stay readable, and the colours cross-fade along with the art when the song changes. `k` turns album colours off: the Apple and Vibrant layouts are then plain white, the Spotify layout uses a neutral grey scheme with white text, and the panels are neutral grey with white text and white buttons. `ENCORE_TINT=off` starts with them off.

## Features
- Toggleable fullscreen with the `f` or `F11` key
- Fully reactive window, all screen elements will be properly scaled/positioned
- Artist-specific background art (See more [here](#obtaining-and-using-artist-background-images-from-spotify))
- Time-synced lyrics from local files, a cache, [LRCLIB](#lyrics) or KuGou, with a [lyrics manager](#lyrics-manager) for picking, editing and re-timing them
- Click or press space to play/pause; `←` / `→` for previous/next track
- Easy to quit with `Esc` or `q`
- Works with MPD or any MPRIS player (see [Players](#players))
- Keyboard shortcuts overlay on `?` or `h` (see [below](#keyboard-shortcuts-overlay))
- Toggle cursor visibility with `c`
- Toggle a useless debug window with `d`
- Export lyric clips as share-ready MP4s (see [below](#exporting-lyric-clips))

### Keys
| Key | Action |
| --- | --- |
| `f` / `F11` | Toggle fullscreen |
| `c` | Toggle cursor visibility |
| `d` | Toggle debug window |
| `Space` (or click) | Play/pause |
| `←` / `→` | Previous / next track |
| `l` | Cycle layouts (Apple → Vibrant → Spotify) |
| `y` | Toggle lyrics |
| `Shift`+`y` | Open/close the [lyrics manager](#lyrics-manager) |
| `t` | Toggle date/clock overlay |
| `b` | Toggle the moving backdrop |
| `a` | Toggle the artist background for the current layout |
| `k` | Toggle album colours |
| `s` | Toggle the previous/next buttons beside the Spotify layout's progress bar |
| `j` | Cycle the Spotify layout's lyric alignment (left → centre → right → right column, left-aligned) |
| `o` | Cycle the Spotify layout's lyric flow (classic → stacked → scrolling) |
| `e` | Open/close the lyric clip export panel |
| `[` / `]` | Mark the clip in-point / out-point at the current playback position |
| `?` / `h` | Show/hide the keyboard shortcuts overlay |
| `Esc` / `q` | Quit (`Esc` closes the shortcuts overlay first when it is open; `q` always quits) |

### Keyboard shortcuts overlay
`?` or `h` opens a "Keyboard shortcuts" cheatsheet over a dimmed backdrop: a dark card with the keys grouped into Playback, Lyrics, Display, Export and App across three columns, each key drawn as an outlined monospace key cap beside its description, with alternative keys separated by a `/` and the keys of a combination joined by a `+`. Press `?` or `h` again, press `Esc`, click the × in the card's corner, or click outside the card to close it; the click does not toggle play/pause. Other keys keep working while it is open. While the export panel or the lyrics manager is open, `?` and `h` do nothing.

## Screenshots
![image](img/1.png)

![image](img/2.png)

![image](img/3.png)

![image](img/4.png)

![image](img/5.png)

![image](img/6.png)

![image](img/7.png)

## Prerequisites
- Rust and `cargo`, to build.
- A playback source: either an MPD server (with `mpc`, which fetches MPD album art), or any MPRIS-capable player. See [Players](#players).
- For MPRIS, a D-Bus session bus, which desktop sessions provide.
- Optionally `ffmpeg`, for [exporting lyric clips](#exporting-lyric-clips).

### Setting up MPD
Skip this if you only use MPRIS players.

| Distribution | Install |
| --- | --- |
| Arch | `sudo pacman -S mpd mpc` |
| Debian/Ubuntu | `sudo apt install mpd mpc` |
| Fedora | `sudo dnf install mpd mpc` |
| openSUSE | `sudo zypper install mpd mpclient` |

Set `music_directory` and at least one `audio_output` in `~/.config/mpd/mpd.conf`:
```
music_directory "~/Music"

audio_output {
    type "pipewire"
    name "PipeWire"
}
```
Then run `systemctl --user enable --now mpd` and queue music with a client such as `mpc` or `ncmpcpp`.

## Installation/Usage
I'm too lazy to figure out how to make a proper cargo package.

Clone the repo, run `cargo build --release`, then create a startup script to execute the binary in the crate directory:
```
#!/usr/bin/env bash
cd <PATH TO CLONED REPO> || exit 1

target/release/encore
```

`cargo install --path .` installs the `encore` binary instead. The window's class (app id) is `encore`, for use in compositor window rules.

### Desktop entry
To start Encore from the GNOME or KDE launcher, install the desktop entry and icon:
```
install -Dm644 encore.desktop ~/.local/share/applications/encore.desktop
install -Dm644 assets/logo.png ~/.local/share/pixmaps/encore.png
```
The entry runs `encore`, so `~/.cargo/bin` must be on `PATH` (as it is after `cargo install --path .` with a standard Rust setup). The icon is not square, so it goes in `pixmaps`, which accepts any size, rather than in a `hicolor` size directory.

Encore starts fullscreen: `Esc` or `q` quits and `f` toggles fullscreen.

### Configuration file
Startup settings live in `~/.config/encore/config.toml` (`$XDG_CONFIG_HOME/encore/config.toml` when `XDG_CONFIG_HOME` is set). On first launch Encore writes a starter file there with every setting commented out at its default; an existing file is never overwritten. Every key is optional:

```toml
layout = "spotify"          # apple | vibrant | spotify
lyrics = true
lyrics_online = true        # LRCLIB and KuGou lookups
lyrics_align = "right-left" # left | center | right | right-left
lyrics_flow = "scrolling"   # classic | stacked | scrolling
motion = true
artist_bg = true            # unset: on for Spotify, off for the others
tint = true
skip_buttons = true
clock = true                # date and clock overlay at startup
export_dir = "~/Videos/encore"
source = "auto"             # auto | mpd | mpris
mpris_player = "spotify"    # substring of the player's bus name; unset: any player

[mpd]
host = "localhost"          # "password@host" for a server with a password
port = 6600
music_dir = "~/Music"
```

Each setting takes the same values as the environment variable of the same meaning below, and an environment variable that is set overrides the file. Settings left out use their defaults. A key that is unknown or has an invalid value is skipped with a warning in the log, and a file that is not valid TOML is ignored as a whole, so a mistake never stops Encore from starting. The file is only read: settings changed with the keyboard while Encore runs last until it closes.

### Environment variables
Environment variables override the [configuration file](#configuration-file). The `ENCORE_*` variables also accept their older `MPD_DISPLAY_*` names (for example `MPD_DISPLAY_LAYOUT`) when the `ENCORE_*` variable is unset.

| Variable | Description |
| --- | --- |
| `MPD_HOST` | MPD server host (unused when `ENCORE_SOURCE=mpris`). Defaults to `localhost`. If your server has a password, format it as `pass@ip`. |
| `MPD_PORT` | MPD server port. Defaults to `6600`. |
| `MPD_MUSIC_DIR` | Your MPD music directory. Enables sidecar lyrics files next to the audio files and audio in exported clips. If unset, `music_directory` is read from `$XDG_CONFIG_HOME/mpd/mpd.conf` (`~/.config/mpd/mpd.conf` when unset), `~/.mpdconf`, or `/etc/mpd.conf`, whichever exists first. |
| `ENCORE_EXPORT_DIR` | Output folder for exported clips. Defaults to `~/Videos/encore`. |
| `ENCORE_LAYOUT` | `apple`, `vibrant` or `spotify`. Defaults to `apple`. |
| `ENCORE_LYRICS` | `on` or `off`. Defaults to `on`. |
| `ENCORE_MOTION` | `on` or `off`: the moving backdrop. Defaults to `on`. |
| `ENCORE_ARTIST_BG` | `on` or `off`: the artist background in every layout. Defaults to on for the Spotify layout and off for the others. |
| `ENCORE_TINT` | `on` or `off`: [album colours](#album-colours) for text, icons and bars. Defaults to `on`. |
| `ENCORE_SKIP_BUTTONS` | `on` or `off`: previous/next buttons beside the Spotify layout's progress bar. Defaults to `on`. |
| `ENCORE_LYRICS_ALIGN` | `left`, `center`, `right` or `right-left` (the right-hand column with left-aligned lines): alignment of the Spotify layout's lyrics. Defaults to `right`. |
| `ENCORE_LYRICS_FLOW` | `classic`, `stacked` or `scrolling`: how the Spotify layout's synced lyrics move. Defaults to `stacked`. |
| `ENCORE_LYRICS_ONLINE` | Set to `0`, `off`, `false` or `no` to disable online lookups (LRCLIB and KuGou) and the lyrics manager's search. Enabled by default. |
| `ENCORE_SOURCE` | `auto`, `mpd` or `mpris` (case-insensitive): [where playback is read from](#players). Defaults to `auto`. |
| `ENCORE_MPRIS_PLAYER` | Case-insensitive part of an MPRIS player's name (for example `spotify`, `firefox`, `vlc` or `mpv`) to follow. Defaults to any player. |
| `ENCORE_CLOCK` | `on` or `off`: the date and clock overlay at startup (`t` toggles it). Defaults to `on`. |

## Exporting lyric clips
Renders a share-ready MP4 of the Apple, Vibrant or Spotify layout (lyrics, album art, title and progress bar) for a chosen part of the current song. Rendering happens offline in the background on the GPU while the display keeps running; with the CPU encoding at the same time, a clip with the moving backdrop takes a few times its own length to export. Requires `ffmpeg` with libx264 and aac.

### Formats
| Format | Resolution |
| --- | --- |
| 16:9 | 1920×1080 |
| 9:16 (default) | 1080×1920 |
| 1:1 | 1080×1080 |

Pick one or several with the **Size** chips; each becomes its own file. Portrait and square have their own layouts, which are also used when the live window has that shape. The Spotify style renders 16:9 only.

### The panel
Press `e` to open the export panel. It has two columns: **Clip** on the left for the range, and **Look** and **Output** on the right, with Cancel and Export below them. Narrow windows stack the columns.

### Look
The **Style** chips (**Dark**, **Vibrant** and **Spotify**) choose the look of the clip: the dark Apple layout over blurred art, the colourful Vibrant layout, or the Spotify layout (with the next song in the corner when the clip reaches the last 10% of the song). The panel starts on the style of the layout on screen. Everything else in the section starts at the display's current setting:
- **Lyrics** (**Left**, **Center**, **Right**) and **Flow** (**Classic**, **Stacked**): the Spotify style's lyric alignment and flow; disabled for the other styles.
- **Motion**: the moving backdrop, or a still one when off.
- **Artist background**: the artist image as the background; it starts at the live setting of the chosen style's layout and is disabled, with a note, when `artists/` has no image for the song's artist.
- **Album colours**: [album colours](#album-colours), or the plain look when off.

Spotify-style clips also follow the display's skip-button setting (`s`).

### Choosing the range
The lines inside the clip are highlighted in the lyric list, and the range slider below it spans the whole song, with a tick at the start of every lyric line. All three controls stay in sync:
- Click the first lyric line, then the last (shift-click also works). The start snaps to 0.5 s before the first line, and the end to the start of the line after the last.
- Drag either handle of the range slider to move that end freely, or drag the highlighted part between them to move the whole range and keep its length. Pressing the bare track brings the nearer handle there. The handles stay at least 0.5 s apart and within the song.
- Nudge the start and end with − and +: 0.5 s per click, 5 s with Shift held.
- Alternatively, mark while listening: `[` sets the in-point and `]` the out-point, then press `e`.
- A clip can be at most 3 minutes long; dragging and nudging keep the range within that.
- Songs without synced lyrics show a note in place of the list; the slider and nudges still work.
- `Esc` closes the panel.

### Quality
The **Quality** chips, **High** and **Small**, pick the encoder settings; the panel remembers the choice until the display restarts and shows a rough size estimate of the files next to the chips. Notes below them say when the Spotify style limits the sizes to 16:9, when there is no artist image, when no audio file was found, and when the range cannot be exported.

| Preset | Video | Audio | Target |
| --- | --- | --- | --- |
| High quality (default) | H.264, CRF 19 | AAC 192 kbit/s | Close to the rendered frames, for keeping the clip or posting where the platform re-encodes anyway |
| Small file | H.264, CRF 24, peak bitrate capped at 3.8 Mbit/s for 1080×1920 (scaled by pixel count for other formats) | AAC 128 kbit/s | Messengers: a 30 s 9:16 clip stays under WhatsApp's 16 MB status limit |

Both presets use x264's `medium` preset with `-tune film` and auto-variance adaptive quantisation (`aq-mode=3`), which keeps dark, soft gradients from flattening into bands. Frames are converted to limited-range BT.709 YUV 4:2:0 with full chroma interpolation and tagged as BT.709, so colours match the display in players and browsers. Exported frames also carry a faint static grain (at most 2 of 255 levels, added over the blurred-art and gradient backdrops only, never over text, art or artist photos) that keeps the encoder from banding them; the live display has none.

### Audio
Audio is cut from the song's file (for MPD, in the MPD music directory; for an MPRIS player, the local file its track points to), with short fades, and can be toggled with the **Audio** switch. The music directory comes from `MPD_MUSIC_DIR`, else `music_directory` in `$XDG_CONFIG_HOME/mpd/mpd.conf` (`~/.config/mpd/mpd.conf` when unset), `~/.mpdconf`, or `/etc/mpd.conf`. If the file cannot be found, the clip is exported without audio and the toast says so. MPRIS tracks that are not local `file://` files (streams, for example) are exported without audio: "no audio: the player's track is not a local file".

### Output
Files are written to `~/Videos/encore/` (override with `ENCORE_EXPORT_DIR`) and named `<Artist> - <Title> (1m05-1m35) 9x16.mp4`, with ` (2)`, ` (3)`, ... appended if the name is taken. Exports run one at a time. A toast in the bottom-right corner shows progress, its × cancels the export, and clicking the "Saved" toast opens the folder.

## Players
Encore reads playback from MPD, from MPRIS players, or from both.

### Source
`source` in the [configuration file](#configuration-file) (or `ENCORE_SOURCE`) chooses where playback comes from. The value is case-insensitive; an unknown value logs a warning and uses `auto`.

| Value | Behaviour |
| --- | --- |
| `auto` (default) | Follows MPD whenever it is reachable, otherwise an MPRIS player. While MPD is down it retries every 5 seconds and switches back once MPD connects. |
| `mpd` | MPD only. Retries every 2 seconds and shows "Connecting to MPD…" until it connects. |
| `mpris` | MPRIS players only. MPD is never contacted. |

With no connection in `auto` or `mpris`, the screen shows "Waiting for a player…".

### Choosing an MPRIS player
`mpris_player` (or `ENCORE_MPRIS_PLAYER`) is a case-insensitive substring of the player's bus name after `org.mpris.MediaPlayer2.`, such as `spotify`, `firefox`, `vlc` or `mpv`. Unset, any player matches. It only affects `auto` and `mpris`.

When several players match, Encore picks:
1. the current player, while it is playing;
2. otherwise a player that is playing;
3. otherwise the last followed player, if it is still present;
4. otherwise the first one alphabetically.

`playerctld` is skipped. An MPD MPRIS bridge such as mpd-mpris or mpDris2 counts as a normal MPRIS player.

### Album art
MPRIS tracks use the player's `mpris:artUrl`: `file://` paths are read from disk, `http(s)` URLs are downloaded (10 second timeout, 20 MB cap) and `data:` URIs are decoded. Without usable art, the backup art is shown. MPD tracks use `mpc readpicture`, falling back to `mpc albumart`.

### Limitations
- MPRIS provides no next-track information, so there is no "Up next" card for MPRIS players.
- Exported clips only have audio for local `file://` tracks.

The debug window (`d`) has a "Player" section showing "Following: MPD" or "Following: MPRIS: <name>", the source setting, the player pin, and the MPD host and port unless the source is `mpris`.

## Lyrics
Lyrics are looked up in this order, and the first hit wins:
1. **Manual lyrics**: lyrics saved in the [lyrics manager](#lyrics-manager).
2. **Sidecar file**: `<song>.lrc`, or `.txt` for unsynced lyrics, next to the audio file. For MPD this requires `MPD_MUSIC_DIR`; for an MPRIS player it applies to local `file://` tracks, and streamed tracks skip this step.
3. **Local folder**: `./lyrics/<artist> - <title>.lrc` (or `.txt`). The primary artist and a bare `<title>` are also tried, and names are matched case-insensitively.
4. **Disk cache**: `~/.cache/encore/lyrics/`
5. **Online**: [LRCLIB](https://lrclib.net) (free, no API key needed), then KuGou. LRCLIB's exact match for the artist and title comes first, then the best synced match from LRCLIB's search, then the best synced match from KuGou's two top results. A synced match must name the same title and artist and be within 5 seconds of the song's length. Plain lyrics from LRCLIB are the last resort, and a song LRCLIB marks as instrumental gets no lyrics.

Online results are saved to the disk cache with a `[source:lrclib]` or `[source:kugou]` line naming the provider, so lyrics keep working offline afterwards. A song nothing is found for gets a `.notfound` marker in the cache and is not looked up online again for 7 days. When a lookup fails on a network error, the current song shows as loading and is retried after 2, 6 and 20 seconds, then no sooner than a minute later. Two lookups run at once: the most recently requested song goes first, and the next queued song is fetched ahead of time after the requested ones.

Synced lyrics use the standard LRC format (`[mm:ss.xx] line`), and the `[offset:+/-ms]` and `[length:mm:ss.xx]` tags are supported. Files without timestamps (and plain-text `.txt` files) are shown as unsynced lyrics. Lyrics files can be UTF-8, UTF-16 with a byte order mark, or Latin-1.

### Lyrics manager
`Shift`+`y` opens the lyrics manager for the current song, a panel in the same style and album colours as the export panel; `Shift`+`y` again, `Esc`, the × in its corner or **Cancel** closes it without saving. While a text field has the keyboard no shortcut works, and `Esc` first leaves the field. If the song changes while it is open, it switches to the new song and clears the search.

- **Current lyrics**: the card at the top shows the lyrics on the display and where they came from, such as "Synced (Cache (LRCLIB))" or "Plain (Manual)", with the timing offset when one is set. Its buttons copy the lyrics to the clipboard, open the timing view, and, for saved manual lyrics, remove them after a confirmation, which looks the song up again.
- **Search**: the field searches LRCLIB and KuGou for its text; left empty it searches for the song's own title and artist. The arrow beside it fills in "Artist - Title". `Enter` or **Search** starts a search. Results are ranked by how well their title and artist match (title words weigh more than the artist, and instrumental or karaoke versions rank lower), then synced before plain, then by the closest length, and identical lyrics are listed once. Each result shows its provider, whether it is synced, its title and artist, its length with the difference from the song's, and whether it is a sure, good or weak match, with its first lines. Providers that fail are noted above the results. Searching needs online lyrics (`lyrics_online`).
- **Add LRC file**: picks a `.lrc` or `.txt` file in the system's file dialog, through the XDG Desktop Portal, with `zenity` or `kdialog` as a fallback when no portal answers. Files can also be dropped anywhere on the window while the manager is open. The file is listed as "File: <name>".
- **Editor**: clicking the current lyrics, a result or the imported file puts its text in the editor, where it can be changed; **Reset** discards the changes. The line under it counts the lyric lines and warns when there are none.
- **Save** stores the editor's text as the song's manual lyrics, which the display shows at once and which take precedence over every other source. They are saved as `~/.cache/encore/lyrics/manual/<artist> - <title>.lrc` when they have timestamps, else `.txt`.

The timing view moves the panel to the side of the window away from the lyrics and dims the layout less, so the lyrics stay readable behind it. It shows the previous, active and next lyric lines at the playback position with their times, and changes apply to the display at once:
- **Offset**: −0.5 s, −0.1 s, +0.1 s and +0.5 s nudges, up to a minute either way, with **Reset**. Positive values show every line later, negative earlier; the offset adds to the file's own `[offset:]` tag.
- **Stretch to song length**: scales every line time by the song's length divided by the lyrics' `[length:]` tag, for sped-up or slowed versions of the recording the lyrics were timed against. It needs synced lyrics with a `[length:]` tag within a factor of four of the song's length.

The offset and stretch are kept per song in `~/.cache/encore/lyrics/manual/<artist> - <title>.json`, whichever source the lyrics come from, and apply to the display and to exported clips.

## Obtaining and using artist background images from Spotify
For Spotify's fullscreen mode, it uses a slightly darkened version of the main artist's profile banner as a background.
If the artist does not have a profile banner, a gradient based on the song's album art is used.

This behaviour is replicated by this program, but the images need to be manually obtained.
If you don't care about Spotify and have your own images you want to use, that is totally cool!
The format of the file should be `<artist name>.jpg` (`.jpeg`, `.png` and `.webp` also work).
The file name should be in **ALL LOWERCASE!**

Obtaining profile banners from Spotify is simple but a little annoying. 
1. navigate to the artist's profile on the Spotify web client. Using Firefox might not work, I had to do this in Chromium.
2. Press `Ctrl+Shift+I` to enter the developer console and inspect the element of the page.
3. Press `Ctrl+F` to open the search prompt. Search for `"background-image:` (Note the quote and colon!). The first result should look like this.
![image](img/background.png)
4. Copy the i.scdn.co link. This is the link to the image. Make sure to *NOT* copy the `&quot;` before and after the link.
![image](img/background2.png)
5. Open this link in your browser. Right click and save it to the `artists/` folder as `<lowercase artist name>.jpg`

### Artist name parsing
In order to display the correct image, this program parses the first artist in the song's artist metadata. There are multiple formats used to separate artist names in this metadata, and improperly parsing this data can cause the program to search for the wrong image file. Current delimeters used are `,_`, `/`, `_&_`, and `;_` (underscores representing spaces). If you encounter issues due to your files using a different format, please open an issue.

## Contributions
If you encounter an issue, open an issue. If you want to contribute... good luck. I'm not too good at rust yet, and this code is rather messy.
