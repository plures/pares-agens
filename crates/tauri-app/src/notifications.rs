use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

use pares_agens_core::Event;

use crate::tray;

static NEXT_NOTIFICATION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationAction {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActionableNotification {
    pub id: String,
    pub title: &'static str,
    pub body: String,
    pub actions: Vec<NotificationAction>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationActionPayload {
    notification_id: String,
    action: String,
    prompt: Option<&'static str>,
}

pub(crate) fn response_content(event: &Event) -> Option<&str> {
    match event {
        Event::ModelResponse { content, .. } | Event::Message { content, .. } => Some(content),
        _ => None,
    }
}

pub(crate) fn derive_actionable_notification(
    content: &str,
    from_background_task: bool,
) -> Option<ActionableNotification> {
    let body = content.trim();
    if body.is_empty() {
        return None;
    }

    let normalized = body.to_ascii_lowercase();
    let (kind, title, actions) = if normalized.contains("ci failure")
        || normalized.contains("ci failed")
        || normalized.contains("workflow failed")
    {
        (
            "ci-failure",
            "CI failure detected",
            vec![
                NotificationAction {
                    id: "view",
                    label: "View",
                },
                NotificationAction {
                    id: "fix",
                    label: "Fix",
                },
            ],
        )
    } else if (normalized.contains("pr ") || normalized.contains("pull request"))
        && normalized.contains("ready for review")
    {
        (
            "pr-ready",
            "PR ready for review",
            vec![
                NotificationAction {
                    id: "approve",
                    label: "Approve",
                },
                NotificationAction {
                    id: "view",
                    label: "View",
                },
            ],
        )
    } else if from_background_task
        || normalized.contains("noteworthy")
        || normalized.contains("worth noting")
    {
        (
            "noteworthy",
            "Noteworthy agent alert",
            vec![NotificationAction {
                id: "view",
                label: "View",
            }],
        )
    } else {
        return None;
    };

    let id = format!(
        "{kind}-{}",
        NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed)
    );
    Some(ActionableNotification {
        id,
        title,
        body: body.to_string(),
        actions,
    })
}

pub(crate) fn maybe_notify(app: &tauri::AppHandle, content: &str, from_background_task: bool) {
    let Some(notification) = derive_actionable_notification(content, from_background_task) else {
        return;
    };

    if let Err(err) = app
        .notification()
        .builder()
        .title(notification.title)
        .body(&notification.body)
        .show()
    {
        tracing::warn!(error = %err, "failed to show desktop notification");
    }

    if let Err(err) = app.emit("actionable-notification", &notification) {
        tracing::warn!(error = %err, "failed to emit actionable notification event");
    }
}

pub(crate) fn handle_action(app: &tauri::AppHandle, notification_id: &str, action: &str) {
    let prompt = match action {
        "fix" => Some("Investigate and fix the failing CI run."),
        "approve" => Some("Review the PR and approve it if everything looks correct."),
        _ => None,
    };

    if matches!(action, "view" | "fix" | "approve") {
        tray::show_and_focus_main_window(app, true);
    } else {
        tracing::warn!(action, "unknown notification action");
        return;
    }

    let payload = NotificationActionPayload {
        notification_id: notification_id.to_string(),
        action: action.to_string(),
        prompt,
    };
    if let Err(err) = app.emit("notification-action", payload) {
        tracing::warn!(error = %err, "failed to emit notification-action event");
    }
}

#[cfg(test)]
mod tests {
    use super::derive_actionable_notification;

    #[test]
    fn ci_failure_message_maps_to_view_and_fix_actions() {
        let n = derive_actionable_notification("CI failure detected on workflow run #42", false)
            .expect("expected notification");
        let action_ids: Vec<&str> = n.actions.iter().map(|a| a.id).collect();
        assert_eq!(n.title, "CI failure detected");
        assert_eq!(action_ids, vec!["view", "fix"]);
    }

    #[test]
    fn pr_ready_message_maps_to_approve_and_view_actions() {
        let n = derive_actionable_notification("PR #18 is ready for review", false)
            .expect("expected notification");
        let action_ids: Vec<&str> = n.actions.iter().map(|a| a.id).collect();
        assert_eq!(n.title, "PR ready for review");
        assert_eq!(action_ids, vec!["approve", "view"]);
    }

    #[test]
    fn background_message_becomes_noteworthy_alert() {
        let n = derive_actionable_notification("Sync finished with drift findings", true)
            .expect("expected notification");
        let action_ids: Vec<&str> = n.actions.iter().map(|a| a.id).collect();
        assert_eq!(n.title, "Noteworthy agent alert");
        assert_eq!(action_ids, vec!["view"]);
    }

    #[test]
    fn regular_chat_message_does_not_trigger_notification() {
        assert!(derive_actionable_notification("Thanks, that worked!", false).is_none());
    }
}
