# Releasing

Releases are built by the GitHub Actions workflow [`.github/workflows/release.yml`](../.github/workflows/release.yml). Pushing a tag whose name starts with `v` builds the packages and publishes them as a GitHub Release.

## Publishing a release
1. Set the new version in `Cargo.toml` (`version = "X.Y.Z"`) and run `cargo build` so `Cargo.lock` picks it up.
2. Commit both files.
3. Tag the commit and push the tag:
   ```
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

The tag must be `v` followed by exactly the `Cargo.toml` version; the workflow fails before building when they differ (for example a `v0.2.0` tag on a commit whose `Cargo.toml` says `0.1.0`). Re-running the workflow for an existing tag updates the files on that release.

## What the workflow produces
The workflow runs on Ubuntu 22.04, so the binary needs glibc 2.35 or newer (Ubuntu 22.04, Linux Mint 21, Debian 12 and later). It builds for x86_64 only. After `cargo test --locked` and `cargo build --release --locked`, it produces:

| File                                  | Contents                                                                                                                                                         |
| ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `encore_X.Y.Z_amd64.deb`              | Debian package built by [cargo-deb](https://github.com/kornelski/cargo-deb) from `[package.metadata.deb]` in `Cargo.toml`: `/usr/bin/encore`, `/usr/share/applications/encore.desktop`, and the README and licences in `/usr/share/doc/encore/` |
| `encore-X.Y.Z-x86_64-linux.tar.gz`    | The `encore` binary, `encore.desktop`, `install.sh` (from `packaging/install.sh`), `README.md`, `LICENSE` and the font licences in `licenses/`                   |
| `SHA256SUMS`                          | SHA-256 checksums of the two files above                                                                                                                         |

On a tag, these files are attached to the GitHub Release, whose notes are generated from the merged pull requests and commits since the previous release. Every run also uploads them as a workflow artifact.

The package's dependencies (`depends` in `[package.metadata.deb]`) list the X11, Wayland, xkbcommon and GL/Vulkan runtime libraries explicitly, because the binary loads them with `dlopen` and cargo-deb's `$auto` detection only sees directly linked libraries. `ffmpeg` is recommended (lyric clip export) and `mpc` and `mpd` are suggested (MPD playback).

## Testing the workflow without publishing
In the repository's **Actions** tab, choose **Release**, then **Run workflow** and pick a branch. The run builds and tests everything but skips the tag check and does not create a release. The `.deb`, tarball and `SHA256SUMS` are attached to the run as an artifact named `encore-X.Y.Z-linux-x86_64`, under the run's **Artifacts** section. From the command line:
```
gh workflow run release.yml --ref <branch>
gh run download --name encore-X.Y.Z-linux-x86_64
```

## Checking a package
```
dpkg-deb --info encore_X.Y.Z_amd64.deb
dpkg-deb --contents encore_X.Y.Z_amd64.deb
sha256sum -c SHA256SUMS
```
