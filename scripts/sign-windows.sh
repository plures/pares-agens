#!/usr/bin/env bash
# Sign a Windows MSI installer via the SignPath REST API.
#
# Required environment variables:
#   SIGNPATH_API_TOKEN        – CI user token from SignPath
#   SIGNPATH_ORGANIZATION_ID  – SignPath organization ID (GUID or slug)
#   SIGNPATH_PROJECT_SLUG     – Project slug configured in SignPath
#   SIGNPATH_POLICY_SLUG      – Signing policy slug (e.g. release-signing)
#
# Optional environment variables:
#   SIGNPATH_ARTIFACT_SLUG    – Artifact configuration slug (default: Tauri-MSI)
#   SIGNPATH_POLL_INTERVAL    – Seconds between status polls (default: 15)
#
# Usage:
#   scripts/sign-windows.sh <path/to/installer.msi>

set -euo pipefail

MSI_PATH="${1:?Usage: sign-windows.sh <installer.msi>}"

: "${SIGNPATH_API_TOKEN:?SIGNPATH_API_TOKEN is required}"
: "${SIGNPATH_ORGANIZATION_ID:?SIGNPATH_ORGANIZATION_ID is required}"
: "${SIGNPATH_PROJECT_SLUG:?SIGNPATH_PROJECT_SLUG is required}"
: "${SIGNPATH_POLICY_SLUG:?SIGNPATH_POLICY_SLUG is required}"

ARTIFACT_SLUG="${SIGNPATH_ARTIFACT_SLUG:-Tauri-MSI}"
POLL_INTERVAL="${SIGNPATH_POLL_INTERVAL:-15}"

API_BASE="https://app.signpath.io/API/v1/${SIGNPATH_ORGANIZATION_ID}"

echo "Submitting ${MSI_PATH} to SignPath for signing (artifact: ${ARTIFACT_SLUG})..."

RESPONSE=$(curl -fsSL \
  -H "Authorization: Bearer ${SIGNPATH_API_TOKEN}" \
  -F "ProjectSlug=${SIGNPATH_PROJECT_SLUG}" \
  -F "SigningPolicySlug=${SIGNPATH_POLICY_SLUG}" \
  -F "ArtifactConfigurationSlug=${ARTIFACT_SLUG}" \
  -F "ArtifactFile=@${MSI_PATH}" \
  "${API_BASE}/SigningRequests")

SIGNING_REQUEST_ID=$(echo "${RESPONSE}" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data['signingRequestId'])
except (json.JSONDecodeError, KeyError) as exc:
    print(f'ERROR: unexpected SignPath response ({exc}): {sys.stdin.read()}', file=sys.stderr)
    sys.exit(1)
")
echo "Signing request submitted: ${SIGNING_REQUEST_ID}"

# Poll until the signing request reaches a terminal state.
while true; do
  STATUS=$(curl -fsSL \
    -H "Authorization: Bearer ${SIGNPATH_API_TOKEN}" \
    "${API_BASE}/SigningRequests/${SIGNING_REQUEST_ID}" \
    | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data['status'])
except (json.JSONDecodeError, KeyError) as exc:
    print(f'ERROR: unexpected SignPath status response ({exc})', file=sys.stderr)
    sys.exit(1)
")

  echo "Status: ${STATUS}"

  case "${STATUS}" in
    Completed)
      break
      ;;
    Failed|Denied|Cancelled)
      echo "ERROR: Signing failed with status '${STATUS}'" >&2
      exit 1
      ;;
    *)
      sleep "${POLL_INTERVAL}"
      ;;
  esac
done

# Overwrite the original artifact with the signed version.
curl -fsSL \
  -H "Authorization: Bearer ${SIGNPATH_API_TOKEN}" \
  "${API_BASE}/SigningRequests/${SIGNING_REQUEST_ID}/SignedArtifact" \
  -o "${MSI_PATH}"

echo "Signed installer saved to: ${MSI_PATH}"
