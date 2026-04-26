//! Personality contracts — structured identity and behavioral rules.
//!
//! A [`PersonalityContract`] defines the agent's identity, tone, and
//! behavioral rules.  Rules are stored in PluresDB and assembled into
//! the system prompt at runtime by [`crate::prompt_builder`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
