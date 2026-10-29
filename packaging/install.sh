#!/bin/sh
# Installs Encore for the current user from an unpacked release tarball:
#   ~/.local/bin/encore
#   ~/.local/share/applications/encore.desktop
# The desktop entry runs the binary by its absolute path, so the launcher
# works even when ~/.local/bin is not on PATH. No root access is needed.
#
# Usage: ./install.sh            install or update
#        ./install.sh --uninstall remove the installed files
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin_dir="$HOME/.local/bin"
app_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
bin="$bin_dir/encore"
desktop="$app_dir/encore.desktop"

refresh_menu() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$app_dir" >/dev/null 2>&1 || true
    fi
}

usage() {
    echo "Usage: $0 [--uninstall]"
}

uninstall() {
    for f in "$bin" "$desktop"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            echo "Removed $f"
        else
            echo "Not installed: $f"
        fi
    done
    refresh_menu
    echo "Encore is uninstalled. Settings and downloaded files in ~/.config/encore and ~/.local/share/encore are kept."
}

install_encore() {
    for f in encore encore.desktop; do
        if [ ! -f "$here/$f" ]; then
            echo "Error: $here/$f is missing; run this script from the unpacked release archive." >&2
            exit 1
        fi
    done

    mkdir -p "$bin_dir" "$app_dir"

    # Copy to a temporary name and rename, so a running copy is not overwritten in place.
    cp "$here/encore" "$bin.tmp.$$"
    chmod 755 "$bin.tmp.$$"
    mv -f "$bin.tmp.$$" "$bin"
    echo "Installed $bin"

    # Desktop entry Exec values are quoted; ", `, $ and \ are escaped inside the quotes.
    # In a sed replacement \ and & are special, so they are escaped once more.
    exec_path=$(printf '%s' "$bin" | sed -e 's/[\\"`$]/\\&/g' -e 's/[\\&]/\\&/g')
    sed "s|^Exec=.*|Exec=\"$exec_path\"|" "$here/encore.desktop" > "$desktop.tmp.$$"
    chmod 644 "$desktop.tmp.$$"
    mv -f "$desktop.tmp.$$" "$desktop"
    echo "Installed $desktop"

    refresh_menu

    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *) echo "Note: $bin_dir is not on PATH. The applications menu entry works regardless; to run 'encore' from a terminal, add $bin_dir to PATH." ;;
    esac
    echo "Encore is installed. Start it from the applications menu or run: $bin"
}

case "${1:-}" in
    "") install_encore ;;
    --uninstall) uninstall ;;
    -h | --help) usage ;;
    *) usage >&2; exit 2 ;;
esac
