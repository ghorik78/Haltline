#!/usr/bin/env bash
# Installs the prebuilt Haltline Zed extension and its debug adapter
# (Linux and macOS). Restart Zed afterwards.
#
#   curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash
#
# Options:
#   --version vX.Y.Z   install a specific release (default: latest)
#   --from DIR         use archives from DIR instead of downloading them
#   --zed-data DIR     Zed data directory (default: detected)
#   --no-adapter       install only the extension (the adapter is then
#                      downloaded by the extension on first use)
set -euo pipefail

usage() {
    cat <<'USAGE'
Usage: install.sh [--version vX.Y.Z] [--from DIR] [--zed-data DIR] [--no-adapter]
  --version vX.Y.Z   install a specific release (default: latest)
  --from DIR         use archives from DIR instead of downloading them
  --zed-data DIR     Zed data directory (default: detected)
  --no-adapter       install only the extension
USAGE
}

repo="ghorik78/Haltline"
version="latest"
from=""
zed_data="${ZED_DATA_DIR:-}"
with_adapter=1

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="$2"; shift 2 ;;
        --from) from="$2"; shift 2 ;;
        --zed-data) zed_data="$2"; shift 2 ;;
        --no-adapter) with_adapter=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

os=$(uname -s)
arch=$(uname -m)
case "$arch" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) echo "unsupported CPU architecture: $arch" >&2; exit 1 ;;
esac
case "$os" in
    Linux) triple="$arch-unknown-linux-musl" ;;
    Darwin) triple="$arch-apple-darwin" ;;
    *) echo "unsupported OS: $os (use install.ps1 on Windows)" >&2; exit 1 ;;
esac

if [ -z "$zed_data" ]; then
    if [ "$os" = Darwin ]; then
        zed_data="$HOME/Library/Application Support/Zed"
    elif [ -d "$HOME/.var/app/dev.zed.Zed/data/zed" ]; then
        zed_data="$HOME/.var/app/dev.zed.Zed/data/zed"   # Flatpak
    else
        zed_data="${XDG_DATA_HOME:-$HOME/.local/share}/zed"
    fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch() {
    local name="$1"
    if [ -n "$from" ]; then
        cp "$from/$name" "$tmp/$name"
        return
    fi
    local url
    if [ "$version" = latest ]; then
        url="https://github.com/$repo/releases/latest/download/$name"
    else
        url="https://github.com/$repo/releases/download/$version/$name"
    fi
    echo "Downloading $url"
    if command -v curl >/dev/null; then
        curl -fsSL -o "$tmp/$name" "$url"
    else
        wget -q -O "$tmp/$name" "$url"
    fi
}

fetch haltline-zed-extension.tar.gz
mkdir -p "$tmp/ext"
tar -xzf "$tmp/haltline-zed-extension.tar.gz" -C "$tmp/ext"
ext_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$tmp/ext/haltline/extension.toml" | head -n1)

installed="$zed_data/extensions/installed"
mkdir -p "$installed"
if [ -L "$installed/haltline" ]; then
    echo "Replacing the dev extension link $installed/haltline"
fi
rm -rf "$installed/haltline"
cp -R "$tmp/ext/haltline" "$installed/haltline"
echo "Installed the extension $ext_version into $installed/haltline"

if [ "$with_adapter" = 1 ]; then
    fetch "haltline-dap-$triple.tar.gz"
    adapter_dir="$zed_data/extensions/work/haltline/haltline-dap-v$ext_version"
    mkdir -p "$adapter_dir"
    tar -xzf "$tmp/haltline-dap-$triple.tar.gz" -C "$adapter_dir"
    chmod +x "$adapter_dir/haltline-dap"
    echo "Installed the debug adapter into $adapter_dir"
fi

echo "Done. Restart Zed (or run 'zed: reload extensions')."
