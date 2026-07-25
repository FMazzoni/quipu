#!/bin/sh
# Install qp (quipu) from the latest GitHub release.
#
#   curl -sSfL https://raw.githubusercontent.com/FMazzoni/quipu/main/install.sh | sh
#
# Set QP_INSTALL_DIR to install somewhere other than ~/.local/bin.
# Re-running upgrades an existing install in place.
#
# Linux always gets the statically linked musl build: it runs on glibc and musl
# distros alike, so there is deliberately no ldd-parsing / libc-detection branch.
set -eu

REPO=FMazzoni/quipu
INSTALL_DIR=${QP_INSTALL_DIR:-$HOME/.local/bin}

die() { echo "install.sh: $*" >&2; exit 1; }

os=$(uname -s)
arch=$(uname -m)

case "$os" in
Linux) os_part=unknown-linux-musl ;;
Darwin) os_part=apple-darwin ;;
*) die "unsupported OS '$os'; prebuilt binaries exist for Linux and macOS only. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

case "$arch" in
x86_64 | amd64) arch_part=x86_64 ;;
aarch64 | arm64) arch_part=aarch64 ;;
*) die "unsupported architecture '$arch'. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

asset="qp-${arch_part}-${os_part}.tar.gz"
url="https://github.com/$REPO/releases/latest/download/$asset"

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

# sha256sum on Linux, shasum -a 256 on macOS; fail closed if neither exists.
if command -v sha256sum >/dev/null 2>&1; then
	sha256() { sha256sum "$1"; }
elif command -v shasum >/dev/null 2>&1; then
	sha256() { shasum -a 256 "$1"; }
else
	die "need sha256sum or shasum to verify the download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT HUP TERM

echo "Downloading $asset ..."
curl -sSfL "$url" -o "$tmp/$asset" || die "download failed: $url"
curl -sSfL "$url.sha256" -o "$tmp/sums" || die "checksum download failed: $url.sha256"

# Take field 1 of each so a bare hash and a "<hash>  <file>" line both work.
want=$(cut -d' ' -f1 <"$tmp/sums")
got=$(sha256 "$tmp/$asset" | cut -d' ' -f1)
[ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $asset (expected '$want', got '$got')"

tar -xzf "$tmp/$asset" -C "$tmp" qp || die "archive did not contain a 'qp' binary"
chmod +x "$tmp/qp"
mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
# Land beside the target first, then rename: an in-place upgrade stays atomic
# and never leaves a half-written qp behind.
mv -f "$tmp/qp" "$INSTALL_DIR/qp.new" || die "could not write to $INSTALL_DIR"
mv -f "$INSTALL_DIR/qp.new" "$INSTALL_DIR/qp" || die "could not install to $INSTALL_DIR/qp"

echo "Installed qp to $INSTALL_DIR/qp"
case ":$PATH:" in
*":$INSTALL_DIR:"*)
	echo "Run 'qp init' in a project to get started."
	;;
*)
	echo "NOTE: $INSTALL_DIR is not on your PATH. Add it to your shell profile:"
	echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
	;;
esac
