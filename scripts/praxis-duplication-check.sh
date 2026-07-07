#!/usr/bin/env bash
# praxis-duplication-check.sh — CI enforcement for ADR-0010
# Detects functions defined in multiple crates with the same signature.
#
# This is a lightweight static check, not a full AST analysis.
# It catches the most common violation: copy-pasting a `fn foo(...)` across crates.
#
# Exit code 0 = clean, 1 = duplicates found.

set -euo pipefail

WORKSPACE_ROOT="${1:-.}"
CRATES_DIR="$WORKSPACE_ROOT/crates"

if [ ! -d "$CRATES_DIR" ]; then
    echo "No crates/ directory found at $WORKSPACE_ROOT — skipping duplication check."
    exit 0
fi

# Extract all `fn <name>(` lines from .rs files, excluding tests and test modules
# Format: crate_name::fn_name
DUPLICATES=$(
    find "$CRATES_DIR" -name "*.rs" -not -path "*/tests/*" -not -path "*/test_*" \
        -exec grep -Hn '^\s*\(pub\s\+\)\?fn [a-z_][a-z0-9_]*(' {} \; 2>/dev/null \
    | sed 's|.*/crates/\([^/]*\)/.*:.*fn \([a-z_][a-z0-9_]*\)(.*|\1::\2|' \
    | sort \
    | awk -F'::' '{
        fn_name = $2;
        crate_name = $1;
        if (fn_name in seen && seen[fn_name] != crate_name) {
            if (!(fn_name in reported)) {
                print "DUPLICATE: fn " fn_name "() found in crates: " seen[fn_name] ", " crate_name;
                reported[fn_name] = 1;
            } else {
                print "  also in: " crate_name;
            }
        }
        seen[fn_name] = crate_name;
    }'
)

# Filter out common false positives:
# - Trait methods (new, default, from, into, fmt, etc.)
# - Common patterns (build, run, start, stop, init, etc.)
# - Short generic names (get, set, len, etc.)
# - Test helper names
COMMON_FN_NAMES="new|default|from|into|fmt|clone|drop|eq|ne|hash|cmp|partial_cmp"
COMMON_FN_NAMES="$COMMON_FN_NAMES|build|run|start|stop|init|main|test|setup|teardown"
COMMON_FN_NAMES="$COMMON_FN_NAMES|render|update|view|name|id|len|is_empty|get|set"
COMMON_FN_NAMES="$COMMON_FN_NAMES|push|pop|clear|iter|next|parse|serialize|deserialize"
COMMON_FN_NAMES="$COMMON_FN_NAMES|display|debug|error|warn|info|trace|log"
COMMON_FN_NAMES="$COMMON_FN_NAMES|open|close|read|write|flush|seek|with_store|with_config"
# Idiomatic builder-setter methods: `fn with_x(mut self, x) -> Self { self.x = Some(x); self }`.
# These recur across unrelated builder structs in different crates (TelegramAdapter,
# Agent, Heartbeat, RadixHandler, App, ...). They are NOT shared operational logic —
# each sets a field on its OWN type — so they cannot (and must not) be hoisted into a
# single home. Same rationale as the with_store/with_config/with_event_spine entries.
COMMON_FN_NAMES="$COMMON_FN_NAMES|with_task_manager|with_state_store|with_session_manager"
COMMON_FN_NAMES="$COMMON_FN_NAMES|with_pipeline_emitter|with_plugin_runtime"
COMMON_FN_NAMES="$COMMON_FN_NAMES|check|validate|verify|matches|search|aggregate|empty"
COMMON_FN_NAMES="$COMMON_FN_NAMES|handles|rules|category|label|evaluate|register"
COMMON_FN_NAMES="$COMMON_FN_NAMES|approve|remove|list|is_complete|is_valid|make_entry|make_event"
COMMON_FN_NAMES="$COMMON_FN_NAMES|in_memory|as_str|chunk_message|with_event_spine"
COMMON_FN_NAMES="$COMMON_FN_NAMES|tool_definitions|default_true|json_error"

# Only flag functions with 15+ character names that appear in multiple crates.
# Short names are overwhelmingly trait impls or constructors.
# Exclude known delegate wrappers (functions that intentionally share a name
# because the consumer delegates to a shared module — see ADR-0010).
KNOWN_DELEGATES="build_nixos_update_command|build_self_update_task|self_update_task_from_env"
KNOWN_DELEGATES="$KNOWN_DELEGATES|build_nixos_update_command_delegates_to_agenda"
# Known debt — tracked for cleanup, excluded from CI gate until resolved.
# TODO: Extract shell_single_quote to a shared utils crate
# TODO: Unify build_system_prompt between cli and core
# TODO: Deduplicate json_error_converted_from_serde test helpers
# NOTE: current_process_rss_kib was extracted into the shared `agens-hostkit`
#       crate (pares_agens_hostkit) — debt resolved, no longer excluded here.
KNOWN_DEBT="shell_single_quote|build_system_prompt|json_error_converted_from_serde"

REAL_DUPLICATES=$(
    echo "$DUPLICATES" \
    | grep -vE "fn ($COMMON_FN_NAMES)\(\)" \
    | grep -vE "fn ($KNOWN_DELEGATES)\(\)" \
    | grep -vE "fn ($KNOWN_DEBT)\(\)" \
    | awk '/^DUPLICATE:/ { match($0, /fn ([a-z_]+)/, a); if (length(a[1]) >= 15) print }' \
    || true
)

if [ -n "$REAL_DUPLICATES" ]; then
    echo "❌ ADR-0010 VIOLATION: Cross-crate function duplication detected"
    echo ""
    echo "$REAL_DUPLICATES"
    echo ""
    echo "Fix: Extract shared logic into a single crate. Consumers delegate, not duplicate."
    echo "See: praxis/decisions/ADR-0010-no-duplicated-operational-logic.md"
    exit 1
else
    echo "✅ ADR-0010: No cross-crate function duplication detected"
    exit 0
fi
