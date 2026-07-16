#!/bin/bash
# ============================================================
# OpenVoice — Cleanup Script
# Removes all tools, symlinks, and build artifacts installed
# during development so you can reclaim disk space when done.
#
# Usage:
#   ./cleanup_installed_tools.sh            — removes everything
#   ./cleanup_installed_tools.sh --keep-rust — keeps Rust toolchain
# ============================================================

set -euo pipefail

KEEP_RUST=false
for arg in "$@"; do
  [[ "$arg" == "--keep-rust" ]] && KEEP_RUST=true
done

echo "🧹 Starting OpenVoice cleanup..."
echo ""

# ──────────────────────────────────────────────
# 1. Homebrew packages installed for this project
# ──────────────────────────────────────────────
echo "▶ Homebrew packages"

for pkg in cmake xcodegen onnxruntime; do
  if brew list --formula 2>/dev/null | grep -q "^${pkg}$"; then
    echo "  Uninstalling ${pkg}..."
    brew uninstall "${pkg}" && echo "  ✓ ${pkg} removed"
  else
    echo "  (${pkg} not installed, skipping)"
  fi
done

if [[ "$KEEP_RUST" == "false" ]]; then
  if brew list --formula 2>/dev/null | grep -q "^rustup$"; then
    echo "  Uninstalling rustup formula..."
    brew uninstall rustup && echo "  ✓ rustup formula removed"
  else
    echo "  (rustup formula not installed, skipping)"
  fi
fi

echo ""

# ──────────────────────────────────────────────
# 2. Homebrew symlinks (Cellar → /usr/local)
# ──────────────────────────────────────────────
echo "▶ Homebrew symlinks"

SYMLINKS=(
  "/usr/local/bin/cmake"
  "/usr/local/bin/rustup"
  "/usr/local/opt/rustup"
  "/usr/local/opt/onnxruntime"
)

for link in "${SYMLINKS[@]}"; do
  if [[ -L "$link" ]]; then
    echo "  Removing symlink: $link"
    rm -f "$link" && echo "  ✓ $link removed"
  else
    echo "  (symlink not found: $link, skipping)"
  fi
done

# Clean up any dangling Homebrew Cellar entries
for formula in cmake xcodegen onnxruntime; do
  cellar_path="/usr/local/Cellar/${formula}"
  if [[ -d "$cellar_path" ]]; then
    echo "  Removing Cellar dir: $cellar_path"
    rm -rf "$cellar_path" && echo "  ✓ Cellar ${formula} removed"
  fi
done

echo ""

# ──────────────────────────────────────────────
# 3. Rust toolchain (~/.cargo, ~/.rustup)
# ──────────────────────────────────────────────
echo "▶ Rust toolchain"

if [[ "$KEEP_RUST" == "true" ]]; then
  echo "  (--keep-rust flag set, skipping Rust toolchain removal)"
else
  if command -v rustup &>/dev/null; then
    echo "  Running 'rustup self uninstall'..."
    rustup self uninstall -y && echo "  ✓ Rust toolchain removed via rustup"
  else
    echo "  rustup not in PATH — removing ~/.cargo and ~/.rustup directly..."
    rm -rf "$HOME/.cargo" "$HOME/.rustup"
    echo "  ✓ ~/.cargo and ~/.rustup removed"
  fi
fi

echo ""

# ──────────────────────────────────────────────
# 4. Project build artifacts (the big ones)
# ──────────────────────────────────────────────
echo "▶ Build artifacts"

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TARGET_DIR="${PROJECT_DIR}/src-tauri/target"
if [[ -d "$TARGET_DIR" ]]; then
  SIZE=$(du -sh "$TARGET_DIR" 2>/dev/null | cut -f1)
  echo "  Removing Rust target/ directory (${SIZE})..."
  rm -rf "$TARGET_DIR"
  echo "  ✓ src-tauri/target/ removed"
else
  echo "  (src-tauri/target/ not found, skipping)"
fi

NODE_DIR="${PROJECT_DIR}/node_modules"
if [[ -d "$NODE_DIR" ]]; then
  SIZE=$(du -sh "$NODE_DIR" 2>/dev/null | cut -f1)
  echo "  Removing node_modules/ (${SIZE})..."
  rm -rf "$NODE_DIR"
  echo "  ✓ node_modules/ removed"
else
  echo "  (node_modules/ not found, skipping)"
fi

DIST_DIR="${PROJECT_DIR}/dist"
if [[ -d "$DIST_DIR" ]]; then
  echo "  Removing dist/ (frontend build)..."
  rm -rf "$DIST_DIR"
  echo "  ✓ dist/ removed"
else
  echo "  (dist/ not found, skipping)"
fi

echo ""

# ──────────────────────────────────────────────
# 5. Downloaded AI models
# ──────────────────────────────────────────────
echo "▶ Downloaded AI models"

MODEL_DIR="$HOME/Library/Application Support/openvoice"
if [[ -d "$MODEL_DIR" ]]; then
  SIZE=$(du -sh "$MODEL_DIR" 2>/dev/null | cut -f1)
  echo "  Removing downloaded models (${SIZE})..."
  rm -rf "$MODEL_DIR"
  echo "  ✓ AI models removed"
else
  echo "  (No downloaded models found, skipping)"
fi

echo ""

# ──────────────────────────────────────────────
# 6. Homebrew download cache
# ──────────────────────────────────────────────
echo "▶ Homebrew cache"
brew cleanup -s 2>/dev/null && echo "  ✓ Homebrew download cache cleaned"

echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║  ✅  Cleanup complete!                               ║"
echo "║  All dev tools, symlinks, and build artifacts        ║"
echo "║  for OpenVoice have been removed.                    ║"
echo "╚══════════════════════════════════════════════════════╝"
echo ""
echo "  Note: ~/.zshrc was NOT modified by this project —"
echo "  no PATH entries need to be removed from it."
