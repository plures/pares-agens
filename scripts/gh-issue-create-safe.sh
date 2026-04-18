#!/bin/bash
# gh-issue-create-safe.sh — ADR-0004 compliant issue creation
# Wraps gh issue create and ensures both label AND type are set.
# Usage: same as `gh issue create` but with mandatory --type flag
#
# Install: alias gh-issue='bash /path/to/gh-issue-create-safe.sh'
# Or: add to pares-agens system prompt as the ONLY way to create issues

set -euo pipefail

# Parse args to check for required fields
HAS_LABEL=false
HAS_TYPE=false
TYPE_VALUE=""
ARGS=("$@")

for i in "${!ARGS[@]}"; do
  case "${ARGS[$i]}" in
    --label|-l) HAS_LABEL=true ;;
    --type) HAS_TYPE=true; TYPE_VALUE="${ARGS[$((i+1))]:-}" ;;
  esac
done

if [ "$HAS_LABEL" = false ]; then
  echo "❌ ADR-0004 VIOLATION: --label is required. Copilot silently cancels without a label."
  echo "   Add: --label enhancement  (or bug, documentation, etc.)"
  exit 1
fi

# Create the issue
ISSUE_URL=$(gh issue create "$@")
ISSUE_NUM=$(echo "$ISSUE_URL" | grep -oP '\d+$')
REPO=$(echo "$ISSUE_URL" | grep -oP 'github\.com/\K[^/]+/[^/]+')

echo "Created: $ISSUE_URL"

# Set type via REST API (gh issue create doesn't support --type)
if [ -n "$TYPE_VALUE" ]; then
  gh api --method PATCH "/repos/$REPO/issues/$ISSUE_NUM" -f "type=$TYPE_VALUE" --silent
  echo "✅ Type set: $TYPE_VALUE"
else
  # Default to Feature for enhancement, Bug for bug
  LABELS=$(gh issue view "$ISSUE_NUM" --repo "$REPO" --json labels --jq '[.labels[].name] | join(",")')
  if echo "$LABELS" | grep -q "bug\|ci-failure"; then
    gh api --method PATCH "/repos/$REPO/issues/$ISSUE_NUM" -f "type=Bug" --silent
    echo "✅ Type auto-set: Bug (from labels)"
  else
    gh api --method PATCH "/repos/$REPO/issues/$ISSUE_NUM" -f "type=Feature" --silent
    echo "✅ Type auto-set: Feature (default)"
  fi
fi
