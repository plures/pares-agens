# QA Runbook — Runtime Capability Verification

## Principle: "Running" ≠ "Capable"

QA must verify **two separate properties** for every deployed agent:

1. **Liveness** — the process is up, responsive, and reports its version.
2. **Capability accuracy** — the agent's stated capabilities (tools, plugins,
   routing mode, model) match what it actually exposes to callers.

A bot can be "running" yet report `Tools: 0 registered` if its status counters
drift from the real runtime state. This class of bug is invisible to liveness
checks alone.

## Channel-Agnostic Verification (C-TEST-002)

All QA capability assertions MUST use a machine-readable surface — **not**
Telegram, Discord, or other chat-specific channels. The canonical surface is the
MCP `runtime_status` tool:

```
POST /mcp (JSON-RPC 2.0)
{ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "runtime_status", "arguments": {} } }
```

### Required assertions

| Field | Assertion |
|-------|-----------|
| `status` | equals `"running"` |
| `version` | non-empty, matches expected release |
| `tool_count` | `> 0` and matches expected minimum |
| `tools` | contains minimum expected set (e.g. `read_file`, `run_command`, `runtime_status`) |
| `components.shell` | equals `"active"` |

### Docker smoke test

```bash
# After container start, call runtime_status via MCP and verify tool visibility
RESPONSE=$(curl -s http://localhost:$PORT/mcp -d '{"method":"tools/call","params":{"name":"runtime_status","arguments":{}}}')
TOOL_COUNT=$(echo "$RESPONSE" | jq '.result.tool_count')
[ "$TOOL_COUNT" -gt 0 ] || exit 1
```

### Windows runtime smoke test

Same logic via the MCP stdio transport or HTTP bridge — assert `tool_count > 0`
and the minimum tool set.

## Dev Test Coverage

The following counters/summaries shown in `/status` or `runtime_status` MUST
have regression tests asserting they equal the real runtime value:

- `tool_count` — must equal `list_tools().len()`
- `tools` array — must contain the minimum expected tool set
- `components` — must reflect actual component availability

See `crates/mcp-server/src/radix_handler.rs` tests:
- `runtime_status_tool_count_matches_list_tools`
- `runtime_status_includes_tool_names`
- `runtime_status_returns_components`

And `crates/cli/src/main.rs`:
- `status_tool_count_matches_full_dispatcher_tool_set`

## Failure Mode Reference

| Symptom | Root cause | Prevented by |
|---------|-----------|--------------|
| "Tools: 0 registered" | Status counted plugin tools only, not built-in | `runtime_status_tool_count_matches_list_tools` |
| Tool list stale after plugin register/deactivate | `tools_list_changed` notification not emitted | `plugin_register_emits_tools_list_changed_notification` |
| Status says "active" but component is None | Hardcoded status string | `runtime_status_returns_components` |
