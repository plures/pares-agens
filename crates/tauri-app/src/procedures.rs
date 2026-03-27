use tauri::State;

use crate::state::AppState;

/// A persisted procedure record stored in AppState.
///
/// Combines the runtime config (name, event type, priority, enabled) with the
/// procedure body text (the DSL definition that will be stored in PluresDB).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureRecord {
    /// Unique procedure name.
    pub name: String,
    /// The event kind this procedure handles (e.g. `"message"`, `"timer"`).
    pub event_type: String,
    /// Execution priority; lower numbers run first.
    pub priority: i32,
    /// Whether the procedure is currently enabled.
    pub enabled: bool,
    /// The procedure DSL body.
    pub body: String,
}

/// A single entry in the procedure execution log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureLogEntry {
    /// Name of the procedure that fired.
    pub procedure_name: String,
    /// ISO-8601 timestamp of when the procedure fired.
    pub fired_at: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// The event kind that triggered this execution.
    pub trigger_event: String,
}

// ── Built-in templates ────────────────────────────────────────────────────

/// Names of the four built-in procedure templates.
const TEMPLATE_NAMES: [&str; 4] = [
    "greeting",
    "scheduled_task",
    "approval_gate",
    "memory_pattern",
];

/// Return the body and event type for a named built-in template.
fn template_body(template: &str) -> Option<(&'static str, &'static str)> {
    match template {
        "greeting" => Some((
            "on message {\n  \
              if event.content contains \"hello\" or \"hi\" {\n    \
                reply \"Hello! How can I help you today?\"\n  \
              }\n\
            }",
            "message",
        )),
        "scheduled_task" => Some((
            "on timer every 60s {\n  \
              emit { kind: \"timer\", label: \"heartbeat\" }\n\
            }",
            "timer",
        )),
        "approval_gate" => Some((
            "on state_change {\n  \
              if state.pending_approvals > 0 {\n    \
                notify \"Pending approval required\"\n  \
              }\n\
            }",
            "state_change",
        )),
        "memory_pattern" => Some((
            "on message {\n  \
              remember {\n    \
                category: \"preference\"\n    \
                content: event.content\n  \
              }\n\
            }",
            "message",
        )),
        _ => None,
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────

/// Return all registered procedures.
#[tauri::command]
pub async fn list_procedures(state: State<'_, AppState>) -> Result<Vec<ProcedureRecord>, String> {
    Ok(state.procedures.lock().await.clone())
}

/// Return a single procedure by name.
#[tauri::command]
pub async fn get_procedure(
    name: String,
    state: State<'_, AppState>,
) -> Result<Option<ProcedureRecord>, String> {
    let procedures = state.procedures.lock().await;
    Ok(procedures.iter().find(|p| p.name == name).cloned())
}

/// Create or update a procedure.
///
/// If a procedure with the same name already exists it is replaced; otherwise
/// the new record is appended.
#[tauri::command]
pub async fn save_procedure(
    record: ProcedureRecord,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut procedures = state.procedures.lock().await;
    if let Some(existing) = procedures.iter_mut().find(|p| p.name == record.name) {
        *existing = record;
    } else {
        procedures.push(record);
    }
    Ok(())
}

/// Enable or disable a procedure by name.
///
/// No-op if no procedure with that name exists.
#[tauri::command]
pub async fn toggle_procedure(
    name: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut procedures = state.procedures.lock().await;
    if let Some(p) = procedures.iter_mut().find(|p| p.name == name) {
        p.enabled = enabled;
    }
    Ok(())
}

/// Return the last `limit` execution log entries, optionally filtered by
/// procedure name.  Entries are returned newest-first.
#[tauri::command]
pub async fn get_procedure_log(
    name: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<ProcedureLogEntry>, String> {
    let log = state.procedure_log.lock().await;
    let cap = limit.unwrap_or(50);
    let entries: Vec<ProcedureLogEntry> = log
        .iter()
        .rev()
        .filter(|e| name.as_deref().is_none_or(|n| e.procedure_name == n))
        .take(cap)
        .cloned()
        .collect();
    Ok(entries)
}

/// Create a new procedure pre-populated from a named built-in template.
///
/// Valid template names: `"greeting"`, `"scheduled_task"`,
/// `"approval_gate"`, `"memory_pattern"`.
#[tauri::command]
pub async fn create_from_template(
    template: String,
    state: State<'_, AppState>,
) -> Result<ProcedureRecord, String> {
    let (body, event_type) = template_body(&template).ok_or_else(|| {
        format!(
            "unknown template \"{template}\"; valid options: {}",
            TEMPLATE_NAMES.join(", ")
        )
    })?;

    // Choose a unique name derived from the template.
    let procedures = state.procedures.lock().await;
    let base_name = template.clone();
    let count = procedures
        .iter()
        .filter(|p| p.name.starts_with(&base_name))
        .count();
    drop(procedures);

    let name = if count == 0 {
        base_name
    } else {
        format!("{base_name}_{count}")
    };

    let record = ProcedureRecord {
        name,
        event_type: event_type.to_string(),
        priority: 0,
        enabled: true,
        body: body.to_string(),
    };

    state.procedures.lock().await.push(record.clone());
    Ok(record)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_body_returns_known_templates() {
        for name in TEMPLATE_NAMES {
            let result = template_body(name);
            assert!(result.is_some(), "template '{name}' should exist");
            let (body, event_type) = result.unwrap();
            assert!(
                !body.is_empty(),
                "template '{name}' body should not be empty"
            );
            assert!(
                !event_type.is_empty(),
                "template '{name}' event_type should not be empty"
            );
        }
    }

    #[test]
    fn template_body_returns_none_for_unknown() {
        assert!(template_body("nonexistent").is_none());
    }

    #[test]
    fn procedure_record_serializes_round_trip() {
        let record = ProcedureRecord {
            name: "test_proc".to_string(),
            event_type: "message".to_string(),
            priority: 0,
            enabled: true,
            body: "on message { reply \"hi\" }".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let de: ProcedureRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(de.name, record.name);
        assert_eq!(de.event_type, record.event_type);
        assert_eq!(de.body, record.body);
        assert!(de.enabled);
    }

    #[test]
    fn procedure_log_entry_serializes_round_trip() {
        let entry = ProcedureLogEntry {
            procedure_name: "greeting".to_string(),
            fired_at: "2026-01-01T00:00:00Z".to_string(),
            duration_ms: 12,
            trigger_event: "message".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let de: ProcedureLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(de.procedure_name, entry.procedure_name);
        assert_eq!(de.duration_ms, 12);
    }
}
