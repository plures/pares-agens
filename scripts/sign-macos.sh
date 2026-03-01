#!/usr/bin/env bash
# Notarize and staple a macOS DMG using Apple Notary Service.
#
# Tauri's built-in notarization (via APPLE_ID/APPLE_PASSWORD env vars) handles
# the common CI path.  Use this script when you need to re-notarize or staple
# an already-signed DMG outside of a Tauri build (e.g. after post-processing).
#
# Required environment variables:
#   APPLE_ID        – Apple Developer account e-mail
#   APPLE_PASSWORD  – App-specific password (not your Apple ID password).
#                     Create one at https://appleid.apple.com/account/manage
#   APPLE_TEAM_ID   – Apple Developer Team ID (10-character string)
#
# Usage:
#   scripts/sign-macos.sh <path/to/installer.dmg>

set -euo pipefail

DMG_PATH="${1:?Usage: sign-macos.sh <installer.dmg>}"

: "${APPLE_ID:?APPLE_ID is required}"
: "${APPLE_PASSWORD:?APPLE_PASSWORD is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"

echo "Submitting ${DMG_PATH} for notarization..."

xcrun notarytool submit "${DMG_PATH}" \
  --apple-id  "${APPLE_ID}" \
  --password  "${APPLE_PASSWORD}" \
  --team-id   "${APPLE_TEAM_ID}" \
  --wait

echo "Stapling notarization ticket to ${DMG_PATH}..."
xcrun stapler staple "${DMG_PATH}"

echo "Notarization complete: ${DMG_PATH}"
