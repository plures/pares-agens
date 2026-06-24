//! Telegram inline keyboard builder for thread operations.
//!
//! Generates `InlineKeyboardMarkup` JSON for thread switching, creation
//! confirmation, and context actions. These are sent alongside Telegram
//! messages to provide interactive thread management.

use serde_json::{json, Value};

/// Builder for thread-related Telegram inline keyboards.
pub struct ThreadKeyboard;

impl ThreadKeyboard {
    /// Build an inline keyboard for thread list (switch buttons).
    ///
    /// `threads` is a slice of `(id, topic)` pairs.
    /// The `active_id` thread is excluded from the buttons.
    /// Returns `Value::Null` if there are no buttons to show.
    pub fn thread_list(threads: &[(String, String)], active_id: &str) -> Value {
        let buttons: Vec<Value> = threads
            .iter()
            .filter(|(id, _)| id != active_id)
            .take(8) // Telegram max ~8 buttons per row is fine; we split into rows of 3
            .map(|(id, topic)| {
                let label = if topic.len() > 20 {
                    format!("{}…", &topic[..17])
                } else {
                    topic.clone()
                };
                json!({
                    "text": label,
                    "callback_data": format!("thread_switch:{}", id)
                })
            })
            .collect();

        if buttons.is_empty() {
            return json!(null);
        }

        // Split into rows of 3
        let rows: Vec<Value> = buttons.chunks(3).map(|chunk| json!(chunk)).collect();

        json!({ "inline_keyboard": rows })
    }

    /// Build a confirmation keyboard for thread creation.
    pub fn confirm_new_thread(topic: &str) -> Value {
        json!({
            "inline_keyboard": [[
                {"text": "✅ Create", "callback_data": format!("thread_create:{}", topic)},
                {"text": "❌ Cancel", "callback_data": "thread_cancel"}
            ]]
        })
    }

    /// Build a thread context keyboard (archive, list actions).
    pub fn thread_context(thread_id: &str) -> Value {
        json!({
            "inline_keyboard": [[
                {"text": "📦 Archive", "callback_data": format!("thread_close:{}", thread_id)},
                {"text": "📋 List all", "callback_data": "thread_list"}
            ]]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_list_multiple_threads() {
        let threads = vec![
            ("t1".to_string(), "topic-a".to_string()),
            ("t2".to_string(), "topic-b".to_string()),
            ("t3".to_string(), "topic-c".to_string()),
            ("t4".to_string(), "topic-d".to_string()),
        ];
        let result = ThreadKeyboard::thread_list(&threads, "t1");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();

        // t1 is excluded (it's active), so we have 3 buttons → fits in 1 row of 3
        assert_eq!(inline.len(), 1);
        let row = inline[0].as_array().unwrap();
        assert_eq!(row.len(), 3);
    }

    #[test]
    fn thread_list_active_excluded() {
        let threads = vec![
            ("active-id".to_string(), "active topic".to_string()),
            ("other-id".to_string(), "other topic".to_string()),
        ];
        let result = ThreadKeyboard::thread_list(&threads, "active-id");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();
        let row = inline[0].as_array().unwrap();

        // Only one button (the non-active thread)
        assert_eq!(row.len(), 1);
        let callback = row[0].get("callback_data").unwrap().as_str().unwrap();
        assert!(callback.contains("other-id"));
        assert!(!callback.contains("active-id"));
    }

    #[test]
    fn thread_list_long_topic_truncated() {
        let threads = vec![(
            "t1".to_string(),
            "This is a very long topic name that exceeds twenty characters".to_string(),
        )];
        let result = ThreadKeyboard::thread_list(&threads, "other");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();
        let row = inline[0].as_array().unwrap();
        let text = row[0].get("text").unwrap().as_str().unwrap();

        // Should be truncated to 17 chars + "…"
        assert!(text.ends_with('…'));
        assert!(text.len() <= 21); // 17 + "…" (3 bytes for …)
    }

    #[test]
    fn thread_list_empty_returns_null() {
        let threads: Vec<(String, String)> = vec![];
        let result = ThreadKeyboard::thread_list(&threads, "active");
        assert!(result.is_null());
    }

    #[test]
    fn thread_list_only_active_returns_null() {
        let threads = vec![("active".to_string(), "only".to_string())];
        let result = ThreadKeyboard::thread_list(&threads, "active");
        assert!(result.is_null());
    }

    #[test]
    fn thread_list_splits_into_rows_of_3() {
        let threads: Vec<(String, String)> = (0..7)
            .map(|i| (format!("t{i}"), format!("topic-{i}")))
            .collect();
        let result = ThreadKeyboard::thread_list(&threads, "active-none");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();

        // 7 threads, 0 excluded → 7 buttons → but capped at 8, split into rows of 3:
        // rows = [3, 3, 1]
        assert_eq!(inline.len(), 3);
        assert_eq!(inline[0].as_array().unwrap().len(), 3);
        assert_eq!(inline[1].as_array().unwrap().len(), 3);
        assert_eq!(inline[2].as_array().unwrap().len(), 1);
    }

    #[test]
    fn confirm_new_thread_keyboard() {
        let result = ThreadKeyboard::confirm_new_thread("my-topic");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();
        assert_eq!(inline.len(), 1);
        let row = inline[0].as_array().unwrap();
        assert_eq!(row.len(), 2);

        let create_btn = &row[0];
        assert!(create_btn
            .get("callback_data")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("my-topic"));
        let cancel_btn = &row[1];
        assert_eq!(
            cancel_btn.get("callback_data").unwrap().as_str().unwrap(),
            "thread_cancel"
        );
    }

    #[test]
    fn thread_context_keyboard() {
        let result = ThreadKeyboard::thread_context("t123");
        let inline = result.get("inline_keyboard").unwrap().as_array().unwrap();
        assert_eq!(inline.len(), 1);
        let row = inline[0].as_array().unwrap();
        assert_eq!(row.len(), 2);

        let archive_btn = &row[0];
        assert!(archive_btn
            .get("callback_data")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("t123"));
    }
}
