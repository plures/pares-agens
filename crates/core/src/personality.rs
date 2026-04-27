//! Personality contracts — structured identity and behavioral rules.
//!
//! A [`PersonalityContract`] defines the agent's identity, tone, and
//! behavioral rules.  Rules are stored in PluresDB and assembled into
//! the system prompt at runtime by [`crate::prompt_builder`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::StateStore;

/// A single behavioral rule that governs agent behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorRule {
    /// Unique identifier for this rule.
    pub id: String,
    /// Category bucket: "communication", "safety", "tools", "memory".
    pub category: String,
    /// Natural-language rule text included in the system prompt.
    pub rule: String,
    /// Priority 1–10 (higher = more important, sorted first in prompt).
    pub priority: u8,
    /// Hard constraint (`true`) vs soft guidance (`false`).
    pub enforced: bool,
}

/// The full personality contract for an agent instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityContract {
    /// Display name shown in identity section.
    pub name: String,
    /// One-line description of the agent.
    pub description: String,
    /// Tone keyword: "direct", "friendly", "professional", etc.
    pub tone: String,
    /// Core behavioral rules (apply to all channels).
    pub rules: Vec<BehaviorRule>,
    /// Channel-specific rule overrides keyed by channel name (e.g. "telegram").
    #[serde(default)]
    pub channel_overrides: HashMap<String, Vec<BehaviorRule>>,
    /// Whether proactive behavior (heartbeats, scheduled tasks) is enabled.
    #[serde(default = "default_true")]
    pub proactive_enabled: bool,
    /// Heartbeat interval in minutes.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_mins: u32,
    /// Start of quiet hours (hour 0-23).
    #[serde(default = "default_quiet_start")]
    pub quiet_hours_start: u8,
    /// End of quiet hours (hour 0-23).
    #[serde(default = "default_quiet_end")]
    pub quiet_hours_end: u8,
    /// Maximum proactive messages per day.
    #[serde(default = "default_max_proactive")]
    pub max_proactive_per_day: u8,
}

fn default_true() -> bool { true }
fn default_heartbeat_interval() -> u32 { 30 }
fn default_quiet_start() -> u8 { 23 }
fn default_quiet_end() -> u8 { 8 }
fn default_max_proactive() -> u8 { 6 }

impl PersonalityContract {
    /// Build the default personality contract seeded on first run.
    pub fn default_contract(name: Option<&str>) -> Self {
        Self {
            name: name.unwrap_or("Pares Agens").to_string(),
            description: "An AI agent built on the plures technology stack.".to_string(),
            tone: "direct".to_string(),
            rules: vec![
                BehaviorRule {
                    id: "core-helpful".into(),
                    category: "communication".into(),
                    rule: "Be genuinely helpful, not performatively helpful. Skip filler words.".into(),
                    priority: 10,
                    enforced: true,
                },
                BehaviorRule {
                    id: "core-opinions".into(),
                    category: "communication".into(),
                    rule: "Have opinions. Disagree when warranted.".into(),
                    priority: 9,
                    enforced: false,
                },
                BehaviorRule {
                    id: "core-resourceful".into(),
                    category: "communication".into(),
                    rule: "Be resourceful before asking. Try to figure it out.".into(),
                    priority: 9,
                    enforced: true,
                },
                BehaviorRule {
                    id: "safety-privacy".into(),
                    category: "safety".into(),
                    rule: "Never share private data from the user's context.".into(),
                    priority: 10,
                    enforced: true,
                },
                BehaviorRule {
                    id: "safety-groups".into(),
                    category: "safety".into(),
                    rule: "In group chats, participate don't dominate.".into(),
                    priority: 8,
                    enforced: true,
                },
                BehaviorRule {
                    id: "safety-errors".into(),
                    category: "safety".into(),
                    rule: "Errors must be surfaced to the user, never silently swallowed.".into(),
                    priority: 10,
                    enforced: true,
                },
                BehaviorRule {
                    id: "comm-concise".into(),
                    category: "communication".into(),
                    rule: "Keep responses concise unless asked for detail.".into(),
                    priority: 7,
                    enforced: false,
                },
                BehaviorRule {
                    id: "comm-reactions".into(),
                    category: "communication".into(),
                    rule: "Use reactions sparingly but genuinely.".into(),
                    priority: 5,
                    enforced: false,
                },
                BehaviorRule {
                    id: "tools-proactive".into(),
                    category: "tools".into(),
                    rule: "Use tools proactively — read files, run commands, search the web — instead of just talking about what you could do.".into(),
                    priority: 9,
                    enforced: true,
                },
                BehaviorRule {
                    id: "comm-honesty".into(),
                    category: "communication".into(),
                    rule: "Admit mistakes and be honest about uncertainty. Never fabricate information.".into(),
                    priority: 10,
                    enforced: true,
                },
                BehaviorRule {
                    id: "comm-formatting".into(),
                    category: "communication".into(),
                    rule: "Prefer bullet lists over tables, use code blocks for commands and file paths.".into(),
                    priority: 7,
                    enforced: false,
                },
            ],
            channel_overrides: {
                let mut overrides = HashMap::new();
                overrides.insert(
                    "telegram".to_string(),
                    vec![
                        BehaviorRule {
                            id: "tg-no-tables".into(),
                            category: "communication".into(),
                            rule: "No markdown tables — use bullet lists.".into(),
                            priority: 8,
                            enforced: true,
                        },
                        BehaviorRule {
                            id: "tg-length".into(),
                            category: "communication".into(),
                            rule: "Keep messages under 2000 chars unless the question demands detail.".into(),
                            priority: 7,
                            enforced: false,
                        },
                        BehaviorRule {
                            id: "tg-reply".into(),
                            category: "communication".into(),
                            rule: "Reply to the original message, not the chat.".into(),
                            priority: 6,
                            enforced: false,
                        },
                        BehaviorRule {
                            id: "tg-concise".into(),
                            category: "communication".into(),
                            rule: "Be extra concise on Telegram — short paragraphs, no walls of text.".into(),
                            priority: 8,
                            enforced: true,
                        },
                        BehaviorRule {
                            id: "tg-code-blocks".into(),
                            category: "communication".into(),
                            rule: "Use code blocks for commands, paths, and config snippets.".into(),
                            priority: 6,
                            enforced: false,
                        },
                    ],
                );
                overrides
            },
            proactive_enabled: true,
            heartbeat_interval_mins: 30,
            quiet_hours_start: 23,
            quiet_hours_end: 8,
            max_proactive_per_day: 6,
        }
    }

    /// Merge a set of rules, replacing any with matching IDs.
    pub fn upsert_rule(&mut self, rule: BehaviorRule) {
        if let Some(existing) = self.rules.iter_mut().find(|r| r.id == rule.id) {
            *existing = rule;
        } else {
            self.rules.push(rule);
        }
    }

    /// Remove a rule by ID. Returns `true` if found and removed.
    pub fn remove_rule(&mut self, id: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        self.rules.len() < before
    }

    /// Format a human-readable summary for display (e.g. `/personality`).
    pub fn display_summary(&self, channel: Option<&str>) -> String {
        let mut out = format!(
            "Personality: {}\nTone: {}\nDescription: {}\n\nCore rules ({}):",
            self.name,
            self.tone,
            self.description,
            self.rules.len()
        );
        let mut sorted = self.rules.clone();
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));
        for r in &sorted {
            let tag = if r.enforced { "enforced" } else { "guidance" };
            out.push_str(&format!(
                "\n• [{}] [p{}] {} — {}",
                tag, r.priority, r.id, r.rule
            ));
        }
        if let Some(ch) = channel {
            if let Some(overrides) = self.channel_overrides.get(ch) {
                out.push_str(&format!("\n\nChannel overrides for '{ch}' ({}):", overrides.len()));
                for r in overrides {
                    let tag = if r.enforced { "enforced" } else { "guidance" };
                    out.push_str(&format!(
                        "\n• [{}] [p{}] {} — {}",
                        tag, r.priority, r.id, r.rule
                    ));
                }
            }
        }
        out
    }
}

/// PluresDB key used to store the personality contract.
pub const PERSONALITY_STATE_KEY: &str = "personality_contract";

// ---------------------------------------------------------------------------
// Personality Documents (SOUL.md, IDENTITY.md, USER.md, etc.)
// ---------------------------------------------------------------------------

/// Prefix for personality document keys in the state store.
const PERSONALITY_DOC_PREFIX: &str = "personality:doc:";

/// Known personality document types.
pub const PERSONALITY_DOC_TYPES: &[&str] = &["soul", "identity", "user", "agents", "heartbeat"];

/// A personality document stored in PluresDB (e.g. SOUL.md content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityDocument {
    /// Document type: "soul", "identity", "user", "agents", "heartbeat".
    pub doc_type: String,
    /// Raw markdown content.
    pub content: String,
    /// Unix timestamp of last update.
    pub updated_at: u64,
}

impl PersonalityDocument {
    /// State store key for this document type.
    pub fn state_key(doc_type: &str) -> String {
        format!("{PERSONALITY_DOC_PREFIX}{doc_type}")
    }
}

/// Store a personality document in a [`StateStore`].
pub async fn store_document(store: &dyn StateStore, doc_type: &str, content: &str) {
    let key = PersonalityDocument::state_key(doc_type);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let doc = PersonalityDocument {
        doc_type: doc_type.to_string(),
        content: content.to_string(),
        updated_at: now,
    };
    if let Ok(value) = serde_json::to_value(&doc) {
        store.set(&key, value).await;
    }
}

/// Retrieve a personality document from a [`StateStore`].
pub async fn get_document(store: &dyn StateStore, doc_type: &str) -> Option<PersonalityDocument> {
    let key = PersonalityDocument::state_key(doc_type);
    let value = store.get(&key).await?;
    serde_json::from_value(value).ok()
}

/// Retrieve all personality documents from a [`StateStore`].
pub async fn get_all_documents(store: &dyn StateStore) -> Vec<PersonalityDocument> {
    let mut docs = Vec::new();
    for doc_type in PERSONALITY_DOC_TYPES {
        if let Some(doc) = get_document(store, doc_type).await {
            docs.push(doc);
        }
    }
    docs
}

/// Seed personality documents from `.md` files in a directory.
///
/// Reads `SOUL.md`, `IDENTITY.md`, `USER.md`, `AGENTS.md`, `HEARTBEAT.md`
/// and stores them in PluresDB if the file is newer than the existing document
/// (or no document exists yet).
pub async fn seed_from_directory(store: &dyn StateStore, dir: &std::path::Path) {
    let mappings: &[(&str, &str)] = &[
        ("SOUL.md", "soul"),
        ("IDENTITY.md", "identity"),
        ("USER.md", "user"),
        ("AGENTS.md", "agents"),
        ("HEARTBEAT.md", "heartbeat"),
        ("SYSTEM-PROMPT.md", "soul"), // legacy fallback
    ];
    for (filename, doc_type) in mappings {
        let path = dir.join(filename);
        if !path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) if !c.trim().is_empty() => c,
            _ => continue,
        };
        let file_modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let should_seed = match get_document(store, doc_type).await {
            None => true,
            Some(existing) => file_modified > existing.updated_at,
        };
        if should_seed {
            store_document(store, doc_type, &content).await;
            tracing::info!(doc_type, filename, "seeded personality document from file");
        }
    }
}

/// Format personality documents for system prompt injection.
///
/// Orders: identity → soul → user → agents → heartbeat.
pub fn format_documents_for_prompt(docs: &[PersonalityDocument]) -> String {
    let order: &[(&str, &str)] = &[
        ("identity", "## Identity"),
        ("soul", "## Soul"),
        ("user", "## About the User"),
        ("agents", "## Agent Guidelines"),
        ("heartbeat", "## Active Tasks"),
    ];
    let mut sections = Vec::new();
    for (doc_type, header) in order {
        if let Some(doc) = docs.iter().find(|d| d.doc_type == *doc_type) {
            sections.push(format!("{header}\n{}", doc.content));
        }
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryStateStore;

    #[test]
    fn default_contract_has_core_rules() {
        let c = PersonalityContract::default_contract(None);
        assert!(!c.rules.is_empty());
        assert!(c.rules.iter().any(|r| r.id == "safety-privacy"));
        assert!(c.channel_overrides.contains_key("telegram"));
    }

    #[test]
    fn upsert_replaces_existing() {
        let mut c = PersonalityContract::default_contract(None);
        let new_rule = BehaviorRule {
            id: "core-helpful".into(),
            category: "communication".into(),
            rule: "Be extremely helpful.".into(),
            priority: 10,
            enforced: true,
        };
        c.upsert_rule(new_rule);
        let found = c.rules.iter().find(|r| r.id == "core-helpful").unwrap();
        assert_eq!(found.rule, "Be extremely helpful.");
    }

    #[test]
    fn remove_rule_works() {
        let mut c = PersonalityContract::default_contract(None);
        assert!(c.remove_rule("core-helpful"));
        assert!(!c.remove_rule("nonexistent"));
    }

    #[tokio::test]
    async fn store_and_retrieve_document() {
        let store = InMemoryStateStore::new();
        store_document(&store, "soul", "Be direct and helpful.").await;
        let doc = get_document(&store, "soul").await.unwrap();
        assert_eq!(doc.doc_type, "soul");
        assert_eq!(doc.content, "Be direct and helpful.");
        assert!(doc.updated_at > 0);
    }

    #[tokio::test]
    async fn get_document_returns_none_when_missing() {
        let store = InMemoryStateStore::new();
        assert!(get_document(&store, "soul").await.is_none());
    }

    #[tokio::test]
    async fn get_all_documents_returns_stored_only() {
        let store = InMemoryStateStore::new();
        store_document(&store, "soul", "Soul content").await;
        store_document(&store, "identity", "Identity content").await;
        let docs = get_all_documents(&store).await;
        assert_eq!(docs.len(), 2);
    }

    #[tokio::test]
    async fn seed_from_directory_creates_documents() {
        let store = InMemoryStateStore::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SOUL.md"), "My soul").unwrap();
        std::fs::write(dir.path().join("USER.md"), "My user").unwrap();
        seed_from_directory(&store, dir.path()).await;
        assert!(get_document(&store, "soul").await.is_some());
        assert!(get_document(&store, "user").await.is_some());
        assert!(get_document(&store, "identity").await.is_none());
    }

    #[tokio::test]
    async fn seed_skips_empty_files() {
        let store = InMemoryStateStore::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SOUL.md"), "   ").unwrap();
        seed_from_directory(&store, dir.path()).await;
        assert!(get_document(&store, "soul").await.is_none());
    }

    #[test]
    fn format_documents_orders_correctly() {
        let docs = vec![
            PersonalityDocument { doc_type: "soul".into(), content: "Soul text".into(), updated_at: 1 },
            PersonalityDocument { doc_type: "identity".into(), content: "Id text".into(), updated_at: 1 },
        ];
        let formatted = format_documents_for_prompt(&docs);
        let id_pos = formatted.find("## Identity").unwrap();
        let soul_pos = formatted.find("## Soul").unwrap();
        assert!(id_pos < soul_pos, "identity should come before soul");
    }
}
