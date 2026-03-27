use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::event::Event;

/// A procedure is a named, async handler that reacts to an event.
#[async_trait]
pub trait Procedure: Send + Sync {
    /// Unique name for this procedure (e.g. `"on_message"`).
    fn name(&self) -> &str;

    /// The event kind this procedure handles (matches [`Event::kind`]).
    fn handles(&self) -> &str;

    /// Execute the procedure in response to the given event.
    async fn execute(&self, event: &Event) -> Vec<Event>;
}

// ---------------------------------------------------------------------------
// ProcedureConfig
// ---------------------------------------------------------------------------

/// Runtime configuration for a single registered procedure.
///
/// Returned by [`ProcedureRegistry::list_configs`] and used by the procedure
/// editor UI to display, edit, and toggle procedures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureConfig {
    /// Unique procedure name.
    pub name: String,
    /// The event kind this procedure handles (e.g. `"message"`).
    pub event_type: String,
    /// Execution priority; lower numbers run first when multiple procedures
    /// handle the same event kind.
    pub priority: i32,
    /// Whether the procedure is currently enabled.
    pub enabled: bool,
}

impl ProcedureConfig {
    /// Create a new config with default priority 0 and `enabled = true`.
    pub fn new(name: impl Into<String>, event_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            event_type: event_type.into(),
            priority: 0,
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ProcedureRegistry
// ---------------------------------------------------------------------------

/// Registry that maps event kinds to their registered procedures.
///
/// Procedures are loaded at startup from PluresDB state and stored here.
/// Multiple procedures may be registered for the same event kind.
///
/// Use [`enable`][Self::enable] / [`disable`][Self::disable] to toggle
/// procedures at runtime, and [`list_configs`][Self::list_configs] to
/// retrieve the current configuration for all registered procedures.
#[derive(Default)]
pub struct ProcedureRegistry {
    procedures: Vec<Box<dyn Procedure>>,
    /// Per-name enabled flag; absent entries default to `true`.
    enabled: HashMap<String, bool>,
    /// Per-name priority; absent entries default to `0`.
    priority: HashMap<String, i32>,
}

impl ProcedureRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a procedure. Procedures are matched by [`Procedure::handles`].
    pub fn register(&mut self, procedure: Box<dyn Procedure>) {
        self.procedures.push(procedure);
    }

    /// Return all procedures that handle the given event kind, skipping
    /// disabled ones, sorted by ascending priority.
    pub fn matching<'a>(&'a self, event_kind: &'a str) -> impl Iterator<Item = &'a dyn Procedure> {
        let mut matched: Vec<&'a dyn Procedure> = self
            .procedures
            .iter()
            .filter(move |p| {
                p.handles() == event_kind && *self.enabled.get(p.name()).unwrap_or(&true)
            })
            .map(|p| p.as_ref())
            .collect();
        matched.sort_by_key(|p| *self.priority.get(p.name()).unwrap_or(&0));
        matched.into_iter()
    }

    /// Enable the procedure with the given name.
    ///
    /// No-op if the name is not registered.
    pub fn enable(&mut self, name: &str) {
        if self.procedures.iter().any(|p| p.name() == name) {
            self.enabled.insert(name.to_string(), true);
        }
    }

    /// Disable the procedure with the given name.
    ///
    /// Disabled procedures are skipped during dispatch.
    pub fn disable(&mut self, name: &str) {
        self.enabled.insert(name.to_string(), false);
    }

    /// Set the execution priority for the procedure with the given name.
    ///
    /// Lower values run first when multiple procedures handle the same event.
    pub fn set_priority(&mut self, name: &str, priority: i32) {
        self.priority.insert(name.to_string(), priority);
    }

    /// Return a snapshot of the configuration for all registered procedures.
    pub fn list_configs(&self) -> Vec<ProcedureConfig> {
        self.procedures
            .iter()
            .map(|p| ProcedureConfig {
                name: p.name().to_string(),
                event_type: p.handles().to_string(),
                priority: *self.priority.get(p.name()).unwrap_or(&0),
                enabled: *self.enabled.get(p.name()).unwrap_or(&true),
            })
            .collect()
    }

    /// Number of registered procedures.
    pub fn len(&self) -> usize {
        self.procedures.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.procedures.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop {
        name: &'static str,
        handles: &'static str,
    }

    #[async_trait]
    impl Procedure for Noop {
        fn name(&self) -> &str {
            self.name
        }
        fn handles(&self) -> &str {
            self.handles
        }
        async fn execute(&self, _: &Event) -> Vec<Event> {
            vec![]
        }
    }

    #[test]
    fn list_configs_reflects_registered_procedures() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));
        registry.register(Box::new(Noop {
            name: "p2",
            handles: "timer",
        }));

        let configs = registry.list_configs();
        assert_eq!(configs.len(), 2);
        assert!(configs.iter().all(|c| c.enabled));
        assert!(configs.iter().all(|c| c.priority == 0));
    }

    #[tokio::test]
    async fn disabled_procedure_is_skipped_during_dispatch() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));
        registry.disable("p1");

        let matched: Vec<_> = registry.matching("message").collect();
        assert!(
            matched.is_empty(),
            "disabled procedure must not be dispatched"
        );
    }

    #[tokio::test]
    async fn re_enabled_procedure_is_dispatched() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));
        registry.disable("p1");
        registry.enable("p1");

        let matched: Vec<_> = registry.matching("message").collect();
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn list_configs_reflects_enabled_state() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));
        registry.disable("p1");

        let configs = registry.list_configs();
        assert!(!configs[0].enabled);
    }

    #[test]
    fn set_priority_reflected_in_list_configs() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));
        registry.set_priority("p1", 10);

        let configs = registry.list_configs();
        assert_eq!(configs[0].priority, 10);
    }

    #[tokio::test]
    async fn matching_returns_procedures_sorted_by_priority() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "high",
            handles: "message",
        }));
        registry.register(Box::new(Noop {
            name: "low",
            handles: "message",
        }));
        registry.set_priority("high", -1);
        registry.set_priority("low", 5);

        let names: Vec<&str> = registry.matching("message").map(|p| p.name()).collect();
        assert_eq!(names, vec!["high", "low"]);
    }

    #[test]
    fn procedure_config_new_defaults() {
        let cfg = ProcedureConfig::new("my_proc", "message");
        assert_eq!(cfg.name, "my_proc");
        assert_eq!(cfg.event_type, "message");
        assert_eq!(cfg.priority, 0);
        assert!(cfg.enabled);
    }

    #[test]
    fn procedure_config_serializes() {
        let cfg = ProcedureConfig::new("my_proc", "message");
        let json = serde_json::to_string(&cfg).unwrap();
        let de: ProcedureConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, de);
    }

    #[tokio::test]
    async fn unregistered_event_kind_returns_no_procedures() {
        let mut registry = ProcedureRegistry::new();
        registry.register(Box::new(Noop {
            name: "p1",
            handles: "message",
        }));

        let matched: Vec<_> = registry.matching("timer").collect();
        assert!(matched.is_empty());
    }
}
