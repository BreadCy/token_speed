#!/bin/sh
# Build and install the Windows tokenspeed binary:
#   - repo deploy copy: tokenspeed/bin/tokenspeed.exe
#   - every installed ZCode plugin cache copy under ~/.zcode/cli/plugins/cache
#   - relaunch the desktop HUD from the deploy copy
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
MANIFEST="$ROOT/tokenspeed-rs/Cargo.toml"
EXE="$ROOT/tokenspeed-rs/target/release/tokenspeed.exe"
DEPLOY="$ROOT/tokenspeed/bin/tokenspeed.exe"

cargo build --release --locked --manifest-path "$MANIFEST"

# The single-instance TCP lock keeps the running process on the OLD binary
# after a file swap — stop it before replacing, then relaunch (MEMORY.md).
taskkill //IM tokenspeed.exe //F >/dev/null 2>&1 || true

cp "$EXE" "$DEPLOY"
echo "deployed $DEPLOY"

for cache_exe in "$HOME"/.zcode/cli/plugins/cache/*/tokenspeed/*/bin/tokenspeed.exe; do
  [ -e "$cache_exe" ] || continue
  cp "$EXE" "$cache_exe"
  echo "updated  $cache_exe"
done

# Start-Process fully detaches the HUD (no handle inheritance): a plain
# `cmd //c start` lets the app hold the caller's stdout pipe open, which hangs
# piped invocations of this script even after everything finished.
powershell -NoProfile -Command "Start-Process -FilePath '$(cygpath -w "$DEPLOY")'"
echo "relaunched HUD from deploy copy"
