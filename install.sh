#!/bin/sh
set -eu

version=${AGENTLAW_VERSION:-latest}
destination=${AGENTLAW_INSTALL_DIR:-"$HOME/.local/share/agentlaw/bin"}
case "$version" in *[!A-Za-z0-9.+-]*) echo 'Invalid version.' >&2; exit 1 ;; esac
case "$version" in latest|v[0-9]*) ;; *) echo 'Use AGENTLAW_VERSION=latest or a v-prefixed version.' >&2; exit 1 ;; esac
case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target=x86_64-unknown-linux-gnu ;;
  Darwin:arm64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  *) echo 'No binary for this platform. Build from source instead.' >&2; exit 1 ;;
esac

base=https://github.com/paranmir/agentlaw/releases
if [ "$version" = latest ]; then base=$base/latest/download; else base=$base/download/$version; fi
archive=agentlaw-$target.tar.gz
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
download() {
  if command -v curl >/dev/null 2>&1; then curl -fSL --retry 2 "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -q "$1" -O "$2"
  else echo 'curl or wget is required.' >&2; exit 1
  fi
}
download "$base/$archive" "$temporary/$archive"
download "$base/SHA256SUMS" "$temporary/SHA256SUMS"
expected=$(awk -v file="$archive" '$2 == file {print $1}' "$temporary/SHA256SUMS")
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$temporary/$archive" | awk '{print $1}')
else actual=$(shasum -a 256 "$temporary/$archive" | awk '{print $1}')
fi
[ -n "$expected" ] && [ "$actual" = "$expected" ] || { echo 'Checksum verification failed; nothing installed.' >&2; exit 1; }
tar -xzf "$temporary/$archive" -C "$temporary" agentlaw agentlaw-worker LICENSE
mkdir -p "$destination"
install -m 755 "$temporary/agentlaw" "$temporary/agentlaw-worker" "$destination/"
install -m 644 "$temporary/LICENSE" "$destination/LICENSE.agentlaw"
printf '\nInstalled Agentlaw to %s\n' "$destination"
printf 'Add this directory to PATH in your shell profile:\n  export PATH="%s:$PATH"\n' "$destination"
printf 'Model setup and harness configuration: https://github.com/paranmir/agentlaw/blob/main/docs/usage.md\n'
