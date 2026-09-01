#!/usr/bin/env bash

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
version="$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"name":"waku","version":"\([^"]*\)".*/\1/p')"
target_triple="$(rustc -vV | sed -n 's/^host: //p')"
package="fintwind-${version}-${target_triple}"
archive="$target_dir/release/$package.tar.gz"
staging="$(mktemp -d)"
trap 'rm -rf -- "$staging"' EXIT

cargo build --locked --release --package waku --bin fintwind --package waku-daemon --bin fintwind-daemon

package_dir="$staging/$package"
install -Dm755 "$target_dir/release/fintwind" "$package_dir/bin/fintwind"
install -Dm755 "$target_dir/release/fintwind-daemon" "$package_dir/bin/fintwind-daemon"
install -Dm644 resources/linux/sh.waku.desktop \
  "$package_dir/share/applications/sh.waku.desktop"
install -Dm644 website/public/app-icon.png \
  "$package_dir/share/icons/hicolor/256x256/apps/sh.waku.png"
install -Dm644 LICENSE "$package_dir/share/licenses/fintwind/LICENSE"

mkdir -p "$(dirname "$archive")"
tar -C "$staging" -czf "$archive" "$package"
printf 'Created %s\n' "$archive"
