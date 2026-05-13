#!/usr/bin/env bash
# scripts/self-update.sh — Single source of truth for the self-update procedure.
#
# The binary invokes this script (not an embedded command) so that pulling
# new source automatically updates the update procedure itself.
#
# This script is designed to be resilient:
# - Dirty working tree (Cargo.lock, target artifacts)
# - Diverged git history
# - Wrong working directory
# - Missing build tools
# - Partial previous runs
#
# Usage: bash scripts/self-update.sh [--no-restart]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HOME="${HOME:-/home/kbristol}"
BIN_DIR="${PARES_BIN_DIR:-$HOME/.local/bin}"
SERVICE_NAME="pares-agens"
PACKAGE_NAME="pares-agens"
NO_RESTART="${1:-}"

cd "$REPO_DIR"

echo "Step 1: Preparing source tree..."
git checkout -- Cargo.lock 2>/dev/null || true
git clean -fd 2>/dev/null || true

echo "Step 2: Pulling latest source..."
git fetch origin main
git reset --hard origin/main

echo "Step 3: Verifying workspace..."
if ! cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"$PACKAGE_NAME\""; then
    echo "ERROR: $PACKAGE_NAME package not found in workspace"
    echo "Available packages:"
    cargo metadata --no-deps --format-version 1 2>/dev/null \
        | grep -o '"name":"[^"]*"' | sed 's/"name":"//;s/"//' | sort
    exit 1
fi

echo "Step 4: Building $PACKAGE_NAME binary..."
cargo build --release -p "$PACKAGE_NAME" 2>&1

echo "Step 5: Installing binary..."
mkdir -p "$BIN_DIR"
cp "target/release/$PACKAGE_NAME" "$BIN_DIR/$PACKAGE_NAME"

if [ "$NO_RESTART" = "--no-restart" ]; then
    echo "Self-update complete. Binary installed (service restart skipped)."
else
    echo "Step 6: Restarting service..."
    sudo systemctl restart "$SERVICE_NAME" 2>/dev/null || echo "Note: systemctl restart failed (may not be running as service)"
    echo "Self-update complete. New binary installed and service restarted."
fi
