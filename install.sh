#!/bin/sh
# Install diavlos from the latest GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh | sh
set -eu

REPO="harisnopen/diavlos"
BIN_DIR="${DIAVLOS_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DIAVLOS_VERSION:-latest}"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$os" in
  linux) os="unknown-linux-gnu" ;;
  darwin) os="apple-darwin" ;;
  *) echo "unsupported OS: $os (on Windows use the npm shim or a release zip)"; exit 1 ;;
esac
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "unsupported architecture: $arch"; exit 1 ;;
esac
target="${arch}-${os}"

if [ "$VERSION" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/diavlos-${target}.tar.gz"
else
  url="https://github.com/${REPO}/releases/download/${VERSION}/diavlos-${target}.tar.gz"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading $url"
curl -fsSL "$url" -o "$tmp/diavlos.tar.gz"
# Every release ships a sigstore bundle next to the archive. Check it when
# cosign is installed.
if command -v cosign >/dev/null 2>&1; then
  curl -fsSL "${url}.sigstore.json" -o "$tmp/bundle.json" && \
  cosign verify-blob --bundle "$tmp/bundle.json" \
    --certificate-identity-regexp "https://github.com/${REPO}/" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    "$tmp/diavlos.tar.gz" && echo "signature ok"
fi
tar -xzf "$tmp/diavlos.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
install -m 755 "$tmp/diavlos" "$BIN_DIR/diavlos"
echo "installed $BIN_DIR/diavlos"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "add it to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
echo "next:  diavlos new <room>   then   diavlos invite <room> <name>"
