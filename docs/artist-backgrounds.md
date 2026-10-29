# Artist backgrounds

The layouts can use an image of the song's artist as their background (toggle with `a`; see [Layouts](layouts.md)). Without an image for the artist, each layout falls back to its own backdrop.

## Where images come from
Encore looks for the artist's image in this order and uses the first one it finds:

1. **Artists folder**: images added by hand to `~/.local/share/encore/artists/` (`$XDG_DATA_HOME/encore/artists/` when `XDG_DATA_HOME` is set). An image here always takes precedence, so adding one replaces a downloaded image. Encore does not create this folder; create it and add images to it (see [File names](#file-names)).
2. **Download cache**: `~/.cache/encore/artists/` (`$XDG_CACHE_HOME/encore/artists/` when `XDG_CACHE_HOME` is set) holds images downloaded earlier and not-found markers. It can be deleted safely; images are downloaded again when needed.
3. **Online**: [TheAudioDB](https://www.theaudiodb.com), then [Deezer](https://www.deezer.com). TheAudioDB's wide fan art is preferred, then its wide thumbnail; from Deezer the artist's 1000×1000 picture is used, unless it is Deezer's grey placeholder. Only an artist whose name matches the song's first artist is accepted (ignoring case, accents and punctuation).

Online lookups only run while the artist background is on for the current layout and no image exists locally. They run in the background, once per artist per session, and a downloaded image fades in over the current song. A downloaded image is saved to the cache as `<artist name>.<ext>`. When neither service has an image, a `<artist name>.notfound` marker is saved instead and the artist is not looked up online again for 7 days. A lookup that fails on a network error writes no marker and is retried a minute later.

To turn online lookups off, set `artists_online = false` in the [configuration file](configuration.md#configuration-file) or `ENCORE_ARTISTS_ONLINE=off`. The artists folder and images already in the cache are still used.

## File names
Name each file after the artist in all lowercase: `<artist name>.jpg`. The extensions `.jpg`, `.jpeg`, `.png` and `.webp` are recognised.

For example, `~/.local/share/encore/artists/kendrick lamar.jpg` is used for songs by Kendrick Lamar.

## Artist name parsing
Encore uses the first artist in the song's artist tag, for files and online lookups alike. Artists are separated in the tag by `, `, `/`, ` & ` or `; ` (each with the spaces shown); everything from the first separator on is ignored, and the remaining name is lowercased and trimmed. For example, `Quality Control; Lil Baby` looks for `quality control.jpg`.

If your files separate artists with another delimiter, please open an issue.

## Spotify-style banners
Spotify's fullscreen mode uses a slightly darkened version of the main artist's profile banner as its background, and a gradient based on the album art when the artist has no banner. Encore reproduces this, but the images have to be collected by hand. Any image can be used; the steps below fetch a Spotify profile banner:

1. Open the artist's profile in the Spotify web client. A Chromium-based browser is more reliable for this than Firefox.
2. Press `Ctrl`+`Shift`+`I` to open the developer tools and inspect the page.
3. Press `Ctrl`+`F` to open the search field and search for `"background-image:` (note the quote and colon). The first result looks like this:

   ![Search result containing the background-image style](images/artist-image-search.png)

4. Copy the `i.scdn.co` link, which is the image's address, without the `&quot;` before and after it:

   ![The copied i.scdn.co link](images/artist-image-link.png)

5. Open the link in the browser, then save the image to the artists folder as `<lowercase artist name>.jpg`.
