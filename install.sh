#!/bin/sh
# Install imkr from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/bastianhenneberg/imkr/main/install.sh | sh
#
# Environment:
#   IMKR_VERSION   the tag to install, e.g. v0.1.0 (default: the latest release)
#   PREFIX         install root, the binary goes to $PREFIX/bin (default: ~/.local)

set -eu

REPO=bastianhenneberg/imkr
BIN=imkr
PREFIX=${PREFIX:-$HOME/.local}
BIN_DIR=$PREFIX/bin

die() {
	printf '%s: %s\n' "$BIN install" "$1" >&2
	exit 1
}

note() {
	printf '%s\n' "$1" >&2
}

# curl or wget, whichever is there.
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
	fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
	fetch_stdout() { wget -qO- "$1"; }
else
	die "needs curl or wget"
fi

os=$(uname -s)
[ "$os" = Linux ] || die "no binaries for $os yet, imkr reads /proc and is Linux only.
Build it from source instead: cargo install --git https://github.com/$REPO --locked"

case $(uname -m) in
x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
*) die "no binaries for $(uname -m) yet.
Build it from source instead: cargo install --git https://github.com/$REPO --locked" ;;
esac

version=${IMKR_VERSION:-}
if [ -z "$version" ]; then
	note "looking up the latest release"
	version=$(fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" |
		sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
	[ -n "$version" ] || die "could not read the latest release, set IMKR_VERSION to a tag"
fi

archive=$BIN-$target.tar.gz
url=https://github.com/$REPO/releases/download/$version/$archive

tmp=$(mktemp -d)
# shellcheck disable=SC2064 # $tmp is known now, and that is what we want to clean up.
trap "rm -rf '$tmp'" EXIT INT TERM

note "downloading $BIN $version for $target"
fetch "$url" "$tmp/$archive" || die "no $archive in release $version: $url"

# The release carries a checksum next to each archive; skip only if nothing can check it.
if fetch "$url.sha256" "$tmp/$archive.sha256" 2>/dev/null; then
	if command -v sha256sum >/dev/null 2>&1; then
		(cd "$tmp" && sha256sum -c "$archive.sha256" >/dev/null) || die "checksum does not match"
	elif command -v shasum >/dev/null 2>&1; then
		(cd "$tmp" && shasum -a 256 -c "$archive.sha256" >/dev/null) || die "checksum does not match"
	else
		note "no sha256sum or shasum, checksum not verified"
	fi
else
	note "release $version has no checksum, not verified"
fi

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/$BIN" ] || die "no $BIN in $archive"

mkdir -p "$BIN_DIR"
# A running server keeps its own open binary, so replacing the file is safe.
install -m 755 "$tmp/$BIN" "$BIN_DIR/$BIN"

# An older tag may not know `version` yet, so fall back to the plain name.
installed=$("$BIN_DIR/$BIN" version 2>/dev/null || echo "$BIN")
note "installed $installed to $BIN_DIR/$BIN"

case ":$PATH:" in
*":$BIN_DIR:"*) ;;
*) note "$BIN_DIR is not in PATH, add it: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

note "a running session moves to the new binary with \`$BIN update\`, panes and all"
