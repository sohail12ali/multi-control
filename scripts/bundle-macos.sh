#!/bin/sh
# Build Multi Control.app with the panel and the daemon side by side.
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build -p multicontrol-macos -p multicontrol-daemon
target_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
app="$root/target/Multi Control.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$target_dir/debug/multicontrol" "$app/Contents/MacOS/multicontrol"
cp "$target_dir/debug/multicontrol-daemon" "$app/Contents/MacOS/multicontrol-daemon"
cp "$root/apps/macos/Info.plist" "$app/Contents/Info.plist"
chmod +x "$app/Contents/MacOS/multicontrol" "$app/Contents/MacOS/multicontrol-daemon"
printf '%s\n' "$app"
