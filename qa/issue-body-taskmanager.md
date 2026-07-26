**Problem:** `crates/cli/src/main.rs`'s `RuntimeAgentFactory::build_agent()` never constructed the agent with a `TaskManager`. A `TaskManager` was only wired into the Telegram `TelegramConfig` (for the `/tasks` command) AFTER `build_agent()` had already run and AFTER the `--stdio` early-return branch had already returned — so:

1. In `--stdio` mode (added in #672), `agent.task_manager` was `None` the entire session.
2. Even in Telegram mode, the agent's own `detect_and_store_promises` (`crates/core/src/agent.rs:2004`) checked `self.task_manager`, which was never set on the `Agent` itself — only on the separate `TelegramConfig`/`TaskManager` used by the `/tasks` slash command. Two different, non-shared `TaskManager` instances existed in the Telegram path (one via the factory-built agent with `task_manager: None`, one constructed fresh at serve-time for `/tasks`), and the agent-side one was always empty.

**User-observed symptom this explains:** pares-agens would say (in conversation) that it would do a list of things ("I'll check X, I'll fix Y..."), but the next turn's `/tasks` command showed an empty list, and the agent had no memory of having committed to anything — because `detect_and_store_promises` silently no-op'd (`if let Some(task_mgr) = &self.task_manager` was always `None` on the agent side) or wrote to a `TaskManager` instance that `/tasks` never read from.

**Verified live (real repro, not simulated):**
```
echo "Please respond with exactly this text and nothing else: I will fix the bug in the login module. I will verify the tests pass." | pares-agens.exe serve --stdio --copilot
```
Before fix: zero `tasks_created`/`task_manager` log lines at all — commitment silently dropped.
After fix: `pares_radix_core::task_manager: Created task: ... task_id=c8608cf7-91eb-4cd8-829f-219baf3d500c` and `agent commitments stored as tasks in TaskManager tasks_created=1`.

**Fix:** Added a `task_manager: Arc<TaskManager>` field to `RuntimeAgentFactory`, constructed once (backed by the shared `CrdtStore`) before any agent is built, wired into `build_agent()` via `Agent::with_task_manager`, and reused (not re-constructed) for the Telegram `/tasks` wiring so both paths share the same underlying task store.

**Related, deeper issue (see #676):** even with this fix, promise/commitment *detection* itself is a fragile regex heuristic (`crates/core/src/agent.rs:1907-1968`) explicitly marked with a `TODO: Route through PxBridge` comment that was never completed — so real-world phrasing that doesn't match the narrow pattern list will still silently fail to create a task (confirmed: an initial repro attempt with natural phrasing produced zero tasks; only exact "I will <verb>..." phrasing matched).

**Evidence:** pre-release build, commit to follow this issue; `qa/transcripts/T-taskbug-1.log` (no match, natural phrasing), `qa/transcripts/T-taskbug-2.log` (before fix, exact phrasing, still zero tasks due to `task_manager: None`), `qa/transcripts/T-taskbug-fixed.log` (after fix, `tasks_created=1`).

**Priority:** P0 — this is the exact bug the user reported experiencing directly (repeated task commitments silently forgotten between turns), not a QA-discovered edge case.
