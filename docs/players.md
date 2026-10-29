# Players
Encore reads playback from MPD, from MPRIS players, or from both. MPD needs `mpc` for album art; MPRIS needs a D-Bus session bus, which desktop sessions provide.

## Setting up MPD
This section only applies when playing through MPD; MPRIS players need no setup.

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

## Source
`source` in the [configuration file](configuration.md#configuration-file) (or `ENCORE_SOURCE`) chooses where playback comes from. The value is case-insensitive; an unknown value logs a warning and uses `auto`.

| Value | Behaviour |
| --- | --- |
| `auto` (default) | Follows MPD whenever it is reachable, otherwise an MPRIS player. While MPD is down it retries every 5 seconds and switches back once MPD connects. |
| `mpd` | MPD only. Retries every 2 seconds and shows "Connecting to MPD…" until it connects. |
| `mpris` | MPRIS players only. MPD is never contacted. |

With no connection in `auto` or `mpris`, the screen shows "Waiting for a player…".

## Choosing an MPRIS player
`mpris_player` (or `ENCORE_MPRIS_PLAYER`) is a case-insensitive substring of the player's bus name after `org.mpris.MediaPlayer2.`, such as `spotify`, `firefox`, `vlc` or `mpv`. Unset, any player matches. It only affects `auto` and `mpris`.

When several players match, Encore picks:
1. the current player, while it is playing;
2. otherwise a player that is playing;
3. otherwise the last followed player, if it is still present;
4. otherwise the first one alphabetically.

`playerctld` is skipped. An MPD MPRIS bridge such as mpd-mpris or mpDris2 counts as a normal MPRIS player.

## Album art
MPRIS tracks use the player's `mpris:artUrl`: `file://` paths are read from disk, `http(s)` URLs are downloaded (10 second timeout, 20 MB cap) and `data:` URIs are decoded. Without usable art, the backup art is shown. MPD tracks use `mpc readpicture`, falling back to `mpc albumart`.

## Limitations
- MPRIS provides no next-track information, so there is no "Up next" card for MPRIS players.
- Exported clips only have audio for local `file://` tracks.

The debug window (`d`) has a "Player" section showing "Following: MPD" or "Following: MPRIS: <name>", the source setting, the player pin, and the MPD host and port unless the source is `mpris`.
