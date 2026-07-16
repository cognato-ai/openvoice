#!/bin/sh
# Cargo `runner` hook: fires on every `cargo run` (which `tauri dev` uses under
# the hood) before the binary is launched.
#
# macOS TCC (Accessibility, Microphone) keys permission grants off the running
# binary's code identity. An unsigned/ad-hoc dev binary gets a fresh identity
# on every rebuild, so a grant made in System Settings silently stops applying
# after the next `cargo build` — this signs with a *fixed* identifier every
# time so the identity (and the grant) stays stable across rebuilds.
set -eu

BIN="$1"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENTITLEMENTS="$SCRIPT_DIR/../Entitlements.plist"

codesign --force --sign - --identifier com.openvoice.app \
  --entitlements "$ENTITLEMENTS" \
  "$BIN" >/dev/null 2>&1 || true

exec "$BIN" "$@"
