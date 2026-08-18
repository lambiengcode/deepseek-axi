#!/usr/bin/env bash
# Installs the latest deepseek-axi release from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/lambiengcode/deepseek-axi/main/install.sh | bash
#
# Override install location with DEEPSEEK_AXI_INSTALL_DIR (default ~/.local/bin).
set -euo pipefail

REPO="lambiengcode/deepseek-axi"
BIN_NAME="deepseek-axi"
INSTALL_DIR="${DEEPSEEK_AXI_INSTALL_DIR:-$HOME/.local/bin}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required but not installed" >&2
    exit 1
  }
}
need curl
need tar

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) platform="apple-darwin" ;;
  Linux) platform="unknown-linux-gnu" ;;
  *)
    echo "error: unsupported OS: $os — deepseek-axi ships prebuilt binaries for macOS and Linux only." >&2
    echo "Build from source instead: git clone https://github.com/$REPO && cd deepseek-axi && cargo build --release" >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64 | amd64) cpu="x86_64" ;;
  arm64 | aarch64) cpu="aarch64" ;;
  *)
    echo "error: unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

target="${cpu}-${platform}"
asset="${BIN_NAME}-${target}.tar.gz"

echo "Fetching latest release..." >&2
# Resolve the tag from the /releases/latest redirect rather than the JSON API.
# The API is rate-limited to 60 requests/hour per IP for unauthenticated
# callers, so anyone behind a shared NAT or on CI can hit a bare 403; the
# redirect has no such limit.
latest_tag="$(curl -fsSI "https://github.com/$REPO/releases/latest" \
  | tr -d '\r' | awk '/^[Ll]ocation:/ {print $2}' | sed 's#.*/tag/##')"
if [ -z "$latest_tag" ]; then
  echo "error: could not determine the latest release — has one been published yet?" >&2
  echo "Check https://github.com/$REPO/releases, or build from source: cargo build --release" >&2
  exit 1
fi

url="https://github.com/$REPO/releases/download/$latest_tag/$asset"
echo "Downloading $asset ($latest_tag)..." >&2

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

if ! curl -fsSL "$url" -o "$tmp_dir/$asset"; then
  echo "error: no prebuilt binary for $target at $url" >&2
  echo "Build from source instead: git clone https://github.com/$REPO && cd deepseek-axi && cargo build --release" >&2
  exit 1
fi

tar -xzf "$tmp_dir/$asset" -C "$tmp_dir"
mkdir -p "$INSTALL_DIR"
mv "$tmp_dir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
chmod +x "$INSTALL_DIR/$BIN_NAME"

echo "Installed $BIN_NAME $latest_tag -> $INSTALL_DIR/$BIN_NAME" >&2

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "" >&2
    echo "$INSTALL_DIR is not on your PATH. Add this to your shell profile:" >&2
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" >&2
    ;;
esac

echo "" >&2
echo "Next: export DEEPSEEK_API_KEY=sk-... (get one at https://platform.deepseek.com/api_keys)" >&2
