//! Built-in event handler procedures.
//!
//! | Procedure | Event kind | Description |
//! |-----------|------------|-------------|
//! | [`OnMessage`] | `message` | Full 6-step LLM pipeline with tool loop and memory capture |
//! | [`OnStateChange`] | `state_change` | Watcher-dispatch for PluresDB key changes |
//! | [`OnTimer`] | `timer` | Named timer actions with optional rescheduling |

/// Full 6-step LLM pipeline procedure for inbound messages.
pub mod on_message;
/// State-change watcher and dispatch procedure.
pub mod on_state_change;
/// Named timer action procedure with optional rescheduling.
pub mod on_timer;

pub use on_message::OnMessage;
pub use on_state_change::OnStateChange;
pub use on_timer::OnTimer;
