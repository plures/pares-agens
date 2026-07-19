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
}
