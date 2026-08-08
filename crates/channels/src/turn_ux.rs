//! Pure turn-UX logic for the Telegram channel: progress-status rendering and
//! callback_data parsing/dispatch. No Telegram transport here — every function
//! is a pure, directly-testable unit (C-TEST-002). Side effects (send/edit
//! message, answer callback, cancel turn) live in `telegram.rs`.

/// A control action decoded from an inline-keyboard `callback_data` string.
///
/// Wire format (matches `approval_keyboard` / the Stop button producers):
/// - `stop:{request_id}`         → [`ControlAction::Stop`]
/// - `approval:yes:{request_id}` → [`ControlAction::Approve`]
/// - `approval:no:{request_id}`  → [`ControlAction::Reject`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAction {
    /// Cooperatively cancel the live turn identified by `request_id`.
    Stop {
        /// The turn being stopped.
        request_id: String,
    },
    /// Approve a pending approval prompt identified by `request_id`.
    Approve {
        /// The approval request being approved.
        request_id: String,
    },
    /// Reject a pending approval prompt identified by `request_id`.
    Reject {
        /// The approval request being rejected.
        request_id: String,
    },
}

impl ControlAction {
    /// The request_id this action targets.
    pub fn request_id(&self) -> &str {
        match self {
            ControlAction::Stop { request_id }
            | ControlAction::Approve { request_id }
            | ControlAction::Reject { request_id } => request_id,
        }
    }
}

/// Parse an inline-keyboard `callback_data` string into a [`ControlAction`].
///
/// Returns `None` for unknown/malformed data (caller must still
/// `answer_callback_query` so the Telegram spinner clears).
pub fn parse_callback(data: &str) -> Option<ControlAction> {
    // Split off the action prefix first; the request_id remainder may itself
    // contain ':' (chat_id:msg_id form), so we must not over-split it.
    let (prefix, rest) = data.split_once(':')?;
    match prefix {
        "stop" => {
            if rest.is_empty() {
                return None;
            }
            Some(ControlAction::Stop {
                request_id: rest.to_string(),
            })
        }
        "approval" => {
            let (decision, id) = rest.split_once(':')?;
            if id.is_empty() {
                return None;
            }
            match decision {
                "yes" => Some(ControlAction::Approve {
                    request_id: id.to_string(),
                }),
                "no" => Some(ControlAction::Reject {
                    request_id: id.to_string(),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Build the `callback_data` for a Stop button targeting `request_id`.
pub fn stop_callback_data(request_id: &str) -> String {
    format!("stop:{request_id}")
}

/// Compact spinner frames cycled by the debounced progress editor. Using a
/// small stable set keeps the status line from looking frozen without the
/// noisy ever-incrementing raw step counter.
pub const SPINNER_FRAMES: [char; 4] = ['⠋', '⠙', '⠹', '⠸'];

/// Render a STABLE single-line turn status.
///
/// - Once real content has streamed (`streamed == true`), this returns the
///   accumulated answer text with a trailing `●` cursor — unchanged behavior.
/// - Before any text streams, it shows a compact spinner frame + the current
///   phase, and the running tool name when known. The raw step counter no
///   longer dominates; `frame` cycles a small spinner so the line breathes.
///
/// `phase` is a short label (e.g. "Working", "Thinking"). `tool_name` is the
/// currently-running tool, if any. `frame` selects the spinner glyph.
pub fn render_turn_status(
    phase: &str,
    tool_name: Option<&str>,
    frame: usize,
    streamed: Option<&str>,
) -> String {
    if let Some(answer) = streamed {
        // Real content is flowing — show the answer with a cursor.
        return format!("{answer}\u{25cf}");
    }
    let spin = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
    match tool_name {
        Some(tool) if !tool.is_empty() => format!("{spin} {phase}… 🔧 {tool}"),
        _ => format!("{spin} {phase}…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_callback ────────────────────────────────────────────────────

    #[test]
    fn parses_stop() {
        assert_eq!(
            parse_callback("stop:abc-123"),
            Some(ControlAction::Stop {
                request_id: "abc-123".into()
            })
        );
    }

    #[test]
    fn parses_approval_yes_no() {
        assert_eq!(
            parse_callback("approval:yes:req-1"),
            Some(ControlAction::Approve {
                request_id: "req-1".into()
            })
        );
        assert_eq!(
            parse_callback("approval:no:req-2"),
            Some(ControlAction::Reject {
                request_id: "req-2".into()
            })
        );
    }

    #[test]
    fn request_id_with_colons_preserved() {
        // request_id itself may contain colons (chat_id:msg_id form).
        assert_eq!(
            parse_callback("stop:12345:678"),
            Some(ControlAction::Stop {
                request_id: "12345:678".into()
            })
        );
        assert_eq!(
            parse_callback("approval:yes:12345:678").map(|a| a.request_id().to_string()),
            Some("12345:678".into())
        );
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert_eq!(parse_callback("bogus:xyz"), None);
        assert_eq!(parse_callback("stop:"), None);
        assert_eq!(parse_callback("approval:maybe:req"), None);
        assert_eq!(parse_callback(""), None);
        assert_eq!(parse_callback("approval:yes:"), None);
    }

    #[test]
    fn stop_callback_roundtrips() {
        let data = stop_callback_data("req-9");
        assert_eq!(
            parse_callback(&data),
            Some(ControlAction::Stop {
                request_id: "req-9".into()
            })
        );
    }

    // ── render_turn_status ────────────────────────────────────────────────

    #[test]
    fn status_shows_spinner_and_phase_when_not_streamed() {
        let s = render_turn_status("Working", None, 0, None);
        assert!(s.contains("Working"));
        assert!(s.starts_with(SPINNER_FRAMES[0]));
        // No ever-incrementing "(step N)" dominance.
        assert!(!s.contains("step"));
    }

    #[test]
    fn status_shows_tool_when_present() {
        let s = render_turn_status("Working", Some("web_search"), 1, None);
        assert!(s.contains("web_search"));
        assert!(s.starts_with(SPINNER_FRAMES[1]));
    }

    #[test]
    fn status_switches_to_answer_with_cursor_when_streamed() {
        let s = render_turn_status("Working", Some("web_search"), 0, Some("Here is the answer"));
        assert_eq!(s, "Here is the answer\u{25cf}");
        assert!(!s.contains("Working"));
        assert!(!s.contains("🔧"));
    }

    #[test]
    fn spinner_frame_wraps() {
        let a = render_turn_status("X", None, 0, None);
        let b = render_turn_status("X", None, SPINNER_FRAMES.len(), None);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_tool_name_falls_back_to_phase_only() {
        let s = render_turn_status("Thinking", Some(""), 2, None);
        assert!(!s.contains("🔧"));
        assert!(s.contains("Thinking"));
    }

    // ── Approval resolve seam (#472) ──────────────────────────────────────
    // Proves the channel-agnostic wiring the Telegram callback relies on:
    // an `approval:yes|no:{token}` callback parses to Approve/Reject, maps to
    // the correct `ApprovalDecision`, and `ApprovalRegistry::resolve(token, ..)`
    // wakes a real registered tool-approval waiter. No Telegram in the loop.

    fn decision_for(action: &ControlAction) -> pares_radix_core::approval::ApprovalDecision {
        match action {
            ControlAction::Approve { .. } => pares_radix_core::approval::ApprovalDecision::Allow,
            _ => pares_radix_core::approval::ApprovalDecision::Deny,
        }
    }

    #[tokio::test]
    async fn approve_callback_resolves_pending_tool_approval_allow() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let (req, pending) = registry.register("run_command", "cargo build").await;
        let data = format!("approval:yes:{}", req.token);
        let action = parse_callback(&data).expect("approval callback must parse");
        assert!(matches!(action, ControlAction::Approve { .. }));
        assert_eq!(action.request_id(), req.token);

        let reg2 = registry.clone();
        let token = req.token.clone();
        let decision = decision_for(&action);
        let resolver = tokio::spawn(async move { reg2.resolve(&token, decision).await });

        let got = pending.wait().await;
        assert!(resolver.await.unwrap(), "resolve woke the waiter");
        assert_eq!(got, pares_radix_core::approval::ApprovalDecision::Allow);
        assert!(got.is_allowed());
        assert_eq!(registry.pending_count().await, 0);
    }

    #[tokio::test]
    async fn deny_callback_aborts_pending_tool_approval() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let (req, pending) = registry.register("run_command", "rm -rf build").await;
        let data = format!("approval:no:{}", req.token);
        let action = parse_callback(&data).expect("approval callback must parse");
        assert!(matches!(action, ControlAction::Reject { .. }));
        let woke = registry.resolve(action.request_id(), decision_for(&action)).await;
        assert!(woke);
        let got = pending.wait().await;
        assert_eq!(got, pares_radix_core::approval::ApprovalDecision::Deny);
        assert!(!got.is_allowed(), "Deny must abort the tool");
    }

    #[tokio::test]
    async fn non_token_id_resolve_is_noop_and_falls_through() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let resolved = registry
            .resolve("turn-request-42", pares_radix_core::approval::ApprovalDecision::Allow)
            .await;
        assert!(!resolved, "unknown/non-token id must be a no-op resolve");
    }

    // ── /approve and /deny slash-command resolve seam ─────────────────────
    // Validates the elevated command path: a token passed to
    // `ApprovalRegistry::resolve` with Allow/Deny wakes the pending waiter
    // identically to the inline-keyboard callback path.

    #[tokio::test]
    async fn slash_approve_resolves_pending_approval() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let (req, pending) = registry.register("deploy", "production").await;

        // Simulate `/approve <token>` command issuing Allow decision.
        let resolved = registry
            .resolve(&req.token, pares_radix_core::approval::ApprovalDecision::Allow)
            .await;
        assert!(resolved, "/approve must resolve a live token");

        let got = pending.wait().await;
        assert_eq!(got, pares_radix_core::approval::ApprovalDecision::Allow);
        assert_eq!(registry.pending_count().await, 0);
    }

    #[tokio::test]
    async fn slash_deny_resolves_pending_approval() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let (req, pending) = registry.register("rm_data", "/var/data").await;

        // Simulate `/deny <token>` command issuing Deny decision.
        let resolved = registry
            .resolve(&req.token, pares_radix_core::approval::ApprovalDecision::Deny)
            .await;
        assert!(resolved, "/deny must resolve a live token");

        let got = pending.wait().await;
        assert_eq!(got, pares_radix_core::approval::ApprovalDecision::Deny);
        assert!(!got.is_allowed());
        assert_eq!(registry.pending_count().await, 0);
    }

    #[tokio::test]
    async fn slash_approve_unknown_token_is_noop() {
        let registry = pares_radix_core::approval::ApprovalRegistry::new();
        let resolved = registry
            .resolve("nonexistent-token", pares_radix_core::approval::ApprovalDecision::Allow)
            .await;
        assert!(!resolved, "unknown token must not resolve");
    }
}
