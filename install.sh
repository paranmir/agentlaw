#!/bin/sh
set -eu

version=${AGENTLAW_VERSION:-latest}
root=${AGENTLAW_ROOT:-"$HOME/Agentlaw"}
case "$root" in /*) ;; *) echo 'AGENTLAW_ROOT must be absolute.' >&2; exit 1 ;; esac
[ "$root" != / ] || { echo 'AGENTLAW_ROOT cannot be the filesystem root.' >&2; exit 1; }
destination=$root/bin
command_dir=$root/command
marker=$root/.agentlaw-layout
if [ "$root" != "$HOME/Agentlaw" ] && [ -f "$HOME/Agentlaw/.agentlaw-layout" ]; then
  echo 'Agentlaw is already installed under ~/Agentlaw; update or migrate that installation.' >&2
  exit 1
fi
existing=$(command -v agentlaw 2>/dev/null || true)
if [ -n "$existing" ] && [ "$existing" != "$destination/agentlaw" ] && [ "$existing" != "$command_dir/agentlaw" ]; then
  echo "Agentlaw is already available at $existing; update or migrate that installation." >&2
  exit 1
fi
if [ -n "${AGENTLAW_HOME:-}" ] && [ "$AGENTLAW_HOME" != "$root/state" ]; then
  echo 'AGENTLAW_HOME selects another installation; preserve and migrate it explicitly.' >&2
  exit 1
fi
if [ -f "$marker" ]; then
  [ "$(cat "$marker")" = agentlaw-managed-layout-v1 ] || {
    echo 'Unrecognized Agentlaw layout marker; no files were changed.' >&2; exit 1;
  }
elif [ -e "$destination" ] || [ -e "$root/state" ] || [ -e "$root/memory" ]; then
  echo 'Existing Agentlaw files without a layout marker require explicit migration.' >&2
  exit 1
fi
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
trap 'rm -rf "$temporary"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
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
tar -xzf "$temporary/$archive" -C "$temporary" agentlaw agentlaw-worker agentlaw-launcher LICENSE
mkdir -p "$root"
lockdir=$root/.install-lock
if ! mkdir "$lockdir" 2>/dev/null; then
  if [ -f "$lockdir/pid" ]; then
    owner=$(cat "$lockdir/pid")
    case "$owner" in *[!0-9]*|'') echo 'Invalid installer lock; inspect the managed root.' >&2; exit 1 ;; esac
    if kill -0 "$owner" 2>/dev/null; then echo 'Another installer is still running.' >&2; exit 1; fi
    rm -f "$lockdir/pid"
    rmdir "$lockdir" || { echo 'Cannot recover the stale installer lock.' >&2; exit 1; }
    mkdir "$lockdir" || { echo 'Another installer acquired the lock.' >&2; exit 1; }
  else
    echo 'Installer lock has no owner; inspect it before retrying.' >&2; exit 1
  fi
fi
printf '%s\n' "$$" > "$lockdir/pid"
unlock_install() { rm -f "$lockdir/pid"; rmdir "$lockdir" 2>/dev/null || true; }
trap 'unlock_install; rm -rf "$temporary"' EXIT

if [ ! -f "$marker" ]; then
  pending=$root/.agentlaw-layout.pending
  if [ -e "$pending" ]; then
    [ -f "$pending" ] && [ "$(cat "$pending")" = agentlaw-managed-layout-v1 ] || {
      echo 'Interrupted layout marker needs inspection.' >&2; exit 1;
    }
  else
    printf 'agentlaw-managed-layout-v1\n' > "$pending"
  fi
  mv "$pending" "$marker"
fi

staged=$root/.bin-staged
previous=$root/.bin-previous
remove_reserved_bundle() {
  bundle=$1
  [ -d "$bundle" ] && [ ! -L "$bundle" ] || {
    echo "Reserved Agentlaw bundle is not an ordinary directory: $bundle" >&2; exit 1;
  }
  for item in "$bundle"/* "$bundle"/.[!.]* "$bundle"/..?*; do
    [ -e "$item" ] || [ -L "$item" ] || continue
    case "${item##*/}" in agentlaw|agentlaw-worker|LICENSE.agentlaw) ;;
      *) echo "Reserved Agentlaw bundle contains an unexpected file: $item" >&2; exit 1 ;;
    esac
    [ -f "$item" ] && [ ! -L "$item" ] || {
      echo "Reserved Agentlaw bundle contains an invalid entry: $item" >&2; exit 1;
    }
  done
  rm -f "$bundle/agentlaw" "$bundle/agentlaw-worker" "$bundle/LICENSE.agentlaw"
  rmdir "$bundle" || { echo "Cannot remove reserved Agentlaw bundle: $bundle" >&2; exit 1; }
}
if [ -d "$previous" ]; then
  if [ ! -d "$destination" ]; then
    if [ -d "$staged" ]; then
      mv "$staged" "$destination" || { mv "$previous" "$destination" || true; echo 'Interrupted swap needs inspection.' >&2; exit 1; }
    else
      mv "$previous" "$destination" || { echo 'Cannot restore the previous bundle.' >&2; exit 1; }
    fi
  fi
  if [ -x "$destination/agentlaw" ] && "$destination/agentlaw" --version >/dev/null 2>&1; then
    remove_reserved_bundle "$previous"
  else
    echo 'Published bundle failed verification; previous bundle preserved.' >&2; exit 1
  fi
fi
if [ -d "$destination" ]; then
  for item in "$destination"/* "$destination"/.[!.]* "$destination"/..?*; do
    [ -e "$item" ] || [ -L "$item" ] || continue
    case "${item##*/}" in agentlaw|agentlaw-worker|LICENSE.agentlaw) ;;
      *) echo "Managed bin contains an unexpected file: $item" >&2; exit 1 ;;
    esac
    [ -f "$item" ] && [ ! -L "$item" ] || {
      echo "Managed bin contains an invalid bundle entry: $item" >&2; exit 1;
    }
  done
fi
if [ -d "$staged" ]; then
  remove_reserved_bundle "$staged"
fi
mkdir "$staged"
install -m 755 "$temporary/agentlaw" "$temporary/agentlaw-worker" "$staged/"
install -m 644 "$temporary/LICENSE" "$staged/LICENSE.agentlaw"
"$staged/agentlaw" --version >/dev/null
"$staged/agentlaw" schema >/dev/null
if [ -d "$destination" ]; then mv "$destination" "$previous"; fi
if ! mv "$staged" "$destination"; then
  if [ -d "$previous" ] && [ ! -d "$destination" ]; then mv "$previous" "$destination" || true; fi
  echo 'Bundle publication failed; recovery material was preserved.' >&2
  exit 1
fi
"$destination/agentlaw" --version >/dev/null || { echo 'Published bundle probe failed; previous bundle retained.' >&2; exit 1; }
if [ -d "$previous" ]; then remove_reserved_bundle "$previous"; fi
mkdir -p "$root/state"
mkdir -p "$command_dir"
if [ ! -e "$command_dir/agentlaw" ]; then
  install -m 755 "$temporary/agentlaw-launcher" "$command_dir/agentlaw"
fi
printf '\nInstalled Agentlaw to %s\n' "$destination"
printf 'Add this directory to PATH in your shell profile:\n  export PATH="%s:$PATH"\n' "$command_dir"
printf 'Model setup and harness configuration: https://github.com/paranmir/agentlaw/blob/main/docs/usage.md\n'
