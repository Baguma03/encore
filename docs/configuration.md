# Configuration

## Configuration file
Startup settings live in `~/.config/encore/config.toml` (`$XDG_CONFIG_HOME/encore/config.toml` when `XDG_CONFIG_HOME` is set). On first launch Encore writes a starter file there with every setting commented out at its default; an existing file is never overwritten. Every key is optional:

```toml
layout = "encore-classic"   # encore-classic | classic | vibrant
lyrics = true
lyrics_online = true        # LRCLIB and KuGou lookups
artists_online = true       # TheAudioDB and Deezer artist image lookups
lyrics_align = "right-left" # left | center | right | right-left
lyrics_flow = "scrolling"   # classic | stacked | scrolling
motion = true
artist_bg = true            # unset: on for Encore Classic, off for the others
tint = true
skip_buttons = true
clock = true                # date and clock overlay at startup
export_dir = "~/Videos/encore"
display_font = "Figtree"    # bundled Figtree, an installed family or a font file
display_fonts = ["Figtree", "Inter", "~/.local/share/fonts/Poppins-Regular.ttf"] # cycled with u
source = "auto"             # auto | mpd | mpris
mpris_player = "spotify"    # substring of the player's bus name; unset: any player

[mpd]
host = "localhost"          # "password@host" for a server with a password
port = 6600
music_dir = "~/Music"
```

Each setting takes the same values as the environment variable of the same meaning ([below](#environment-variables)), and an environment variable that is set overrides the file. Settings left out use their defaults. A key that is unknown or has an invalid value is skipped with a warning in the log, and a file that is not valid TOML is ignored as a whole, so a mistake never stops Encore from starting. The file is only read: settings changed with the keyboard while Encore runs last until it closes.

## Environment variables
Environment variables override the [configuration file](#configuration-file). The `ENCORE_*` variables also accept an `MPD_DISPLAY_*` spelling (for example `MPD_DISPLAY_LAYOUT`) when the `ENCORE_*` variable is unset.

| Variable | Description |
| --- | --- |
| `MPD_HOST` | MPD server host (unused when `ENCORE_SOURCE=mpris`). Defaults to `localhost`. For a server with a password, use `password@host`. |
| `MPD_PORT` | MPD server port. Defaults to `6600`. |
| `MPD_MUSIC_DIR` | The MPD music directory. Enables sidecar lyrics files next to the audio files and audio in exported clips. If unset, `music_directory` is read from `$XDG_CONFIG_HOME/mpd/mpd.conf` (`~/.config/mpd/mpd.conf` when unset), `~/.mpdconf`, or `/etc/mpd.conf`, whichever exists first. |
| `ENCORE_EXPORT_DIR` | Output folder for exported clips. Defaults to `~/Videos/encore`. |
| `ENCORE_LAYOUT` | `encore-classic`, `classic` or `vibrant`. Defaults to `encore-classic`. |
| `ENCORE_LYRICS` | `on` or `off`. Defaults to `on`. |
| `ENCORE_MOTION` | `on` or `off`: the moving backdrop. Defaults to `on`. |
| `ENCORE_ARTIST_BG` | `on` or `off`: the artist background in every layout. Defaults to on for the Encore Classic layout and off for the others. |
| `ENCORE_TINT` | `on` or `off`: [album colours](layouts.md#album-colours) for text, icons and bars. Defaults to `on`. |
| `ENCORE_SKIP_BUTTONS` | `on` or `off`: previous/next buttons beside the Encore Classic layout's progress bar. Defaults to `on`. |
| `ENCORE_LYRICS_ALIGN` | `left`, `center`, `right` or `right-left` (the right-hand column with left-aligned lines): alignment of the Encore Classic layout's lyrics. Defaults to `right`. |
| `ENCORE_LYRICS_FLOW` | `classic`, `stacked` or `scrolling`: how the Encore Classic layout's synced lyrics move. Defaults to `stacked`. |
| `ENCORE_ARTISTS_ONLINE` | Set to `0`, `off`, `false` or `no` to disable online [artist image](artist-backgrounds.md) lookups (TheAudioDB and Deezer). Enabled by default. |
| `ENCORE_LYRICS_ONLINE` | Set to `0`, `off`, `false` or `no` to disable online lookups (LRCLIB and KuGou) and the lyrics manager's search. Enabled by default. |
| `ENCORE_SOURCE` | `auto`, `mpd` or `mpris` (case-insensitive): [where playback is read from](players.md#source). Defaults to `auto`. |
| `ENCORE_MPRIS_PLAYER` | Case-insensitive part of an MPRIS player's name (for example `spotify`, `firefox`, `vlc` or `mpv`) to follow. Defaults to any player. |
| `ENCORE_CLOCK` | `on` or `off`: the date and clock overlay at startup (`t` toggles it). Defaults to `on`. |
| `ENCORE_DISPLAY_FONT` | The [display font](#display-fonts) at startup. Defaults to the first entry of `ENCORE_DISPLAY_FONTS`, else `Figtree`. |
| `ENCORE_DISPLAY_FONTS` | Comma-separated [display fonts](#display-fonts) that `u` and `Shift`+`u` cycle through, for example `Figtree,Inter,~/fonts/Poppins-Regular.ttf`. Defaults to `Figtree` and the startup font. |

## Display fonts
The [display font](layouts.md#display-font) sets the now playing display and exported clips; panels, overlays and notifications always use the bundled Lexend Deca. `display_font` picks the font at startup and `display_fonts` lists the fonts `u` cycles through (`Shift`+`u` goes backwards). Each entry is one of:

- `Figtree`: the bundled font, the default.
- The family name of an installed font, such as `Inter` or `DejaVu Sans`, found with fontconfig's `fc-match` (from the `fontconfig` package, present on most desktops). Its regular and bold styles are used; a family without a bold style uses its regular one for both, and a variable font is set at weights 400 and 700. A name fontconfig does not know is skipped with a warning in the log, rather than replaced by a look-alike, and without `fc-match` only `Figtree` and font files work.
- The path to a `.ttf` or `.otf` file (also `.ttc` and `.otc` collections, which use their first font), with `~` for the home directory. A file whose name contains `Regular` takes its bold face from the file beside it with `Bold` in its place, such as `Poppins-Bold.ttf` next to `Poppins-Regular.ttf`; otherwise the one file serves for both.

```toml
display_font = "Inter"
display_fonts = ["Figtree", "Inter", "~/.local/share/fonts/Poppins-Regular.ttf"]
```

When `display_fonts` is unset, the list holds `Figtree` and `display_font`; a list without `display_font` gets it added at the front. In `ENCORE_DISPLAY_FONTS` the entries are separated by commas, so a file whose path contains a comma can only be listed in the configuration file. Entries that cannot be used are skipped, and an unusable startup font falls back to Figtree. The startup font loads before the window first draws; the other entries are looked up in the background, and switching to one still loading waits for it. The font chosen with `u` lasts until Encore closes.
