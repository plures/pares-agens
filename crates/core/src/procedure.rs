use std::collections::HashMap;

use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

/// Semantic version of the stable `ProcedureRegistry` public API.
pub const PROCEDURE_REGISTRY_API_VERSION: &str = "1.0.0";

/// Versioned metadata for loading a procedure definition from a plugin package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureDefinition {
    /// Unique procedure name.
    pub name: String,
    /// The event kind this procedure handles (e.g. `"message"`).
    pub event_type: String,
    /// Semantic version of this procedure definition.
    pub version: String,
    /// Minimum compatible `ProcedureRegistry` API version this procedure needs.
    pub registry_api_version: String,
}

impl ProcedureDefinition {
    /// Create a new definition with stable defaults.
    ///
    /// Defaults:
    /// - `version = "1.0.0"`
    /// - `registry_api_version = PROCEDURE_REGISTRY_API_VERSION`
    pub fn new(name: impl Into<String>, event_type: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            event_type: event_type.into(),
            version: "1.0.0".to_string(),
            registry_api_version: PROCEDURE_REGISTRY_API_VERSION.to_string(),
        }
    }
}

/// Errors returned when loading semver-versioned procedure definitions.
#[derive(Debug, Error)]
pub enum ProcedureLoadError {
    /// Procedure definition `version` is not valid semantic versioning.
    #[error("procedure '{name}' has invalid semantic version '{version}': {source}")]
    InvalidProcedureVersion {
        /// Name of the failing procedure definition.
        name: String,
        /// Raw invalid version string.
        version: String,
        /// Parse failure details.
        source: semver::Error,
    },
    /// Required registry API version is not valid semantic versioning.
    #[error("procedure '{name}' has invalid registry API version '{version}': {source}")]
    InvalidRegistryApiVersion {
        /// Name of the failing procedure definition.
        name: String,
        /// Raw invalid API version string.
        version: String,
        /// Parse failure details.
        source: semver::Error,
    },
    /// Procedure definition does not match the loaded implementation.
    #[error(
        "procedure implementation mismatch: definition ('{definition_name}', '{definition_event_type}') vs implementation ('{implementation_name}', '{implementation_event_type}')"
    )]
    DefinitionMismatch {
        /// Name declared in the definition.
        definition_name: String,
        /// Event type declared in the definition.
        definition_event_type: String,
        /// Name returned by [`Procedure::name`].
        implementation_name: String,
        /// Event type returned by [`Procedure::handles`].
        implementation_event_type: String,
    },
    /// Loaded procedure requires a newer or incompatible registry API version.
    #[error(
        "procedure '{name}' requires registry API {required}, current API is {current} (incompatible)"
    )]
    IncompatibleRegistryApi {
        /// Name of the procedure definition.
        name: String,
        /// Registry API version requested by the definition.
        required: String,
        /// Current registry API version.
        current: String,
    },
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

    /// Stable semantic version for this registry interface.
    pub fn api_version(&self) -> &'static str {
        PROCEDURE_REGISTRY_API_VERSION
    }

    /// Load a versioned procedure definition and enforce compatibility checks.
    ///
    /// This method preserves backward compatibility:
    /// - [`register`][Self::register] is still available and unchanged.
    /// - New plugin/procedure loaders should prefer this method to validate
    ///   semantic versions and API compatibility.
    pub fn load_definition(
        &mut self,
        definition: ProcedureDefinition,
        procedure: Box<dyn Procedure>,
    ) -> Result<(), ProcedureLoadError> {
        let impl_name = procedure.name().to_string();
        let impl_event_type = procedure.handles().to_string();

        if definition.name != impl_name || definition.event_type != impl_event_type {
            return Err(ProcedureLoadError::DefinitionMismatch {
                definition_name: definition.name,
                definition_event_type: definition.event_type,
                implementation_name: impl_name,
                implementation_event_type: impl_event_type,
            });
        }

        let _procedure_version = Version::parse(&definition.version).map_err(|source| {
            ProcedureLoadError::InvalidProcedureVersion {
                name: definition.name.clone(),
                version: definition.version.clone(),
                source,
            }
        })?;
        let required_registry_api =
            Version::parse(&definition.registry_api_version).map_err(|source| {
                ProcedureLoadError::InvalidRegistryApiVersion {
                    name: definition.name.clone(),
                    version: definition.registry_api_version.clone(),
                    source,
                }
            })?;
        let current_registry_api = Version::parse(PROCEDURE_REGISTRY_API_VERSION)
            .expect("PROCEDURE_REGISTRY_API_VERSION must be valid semver");

        if !is_semver_compatible(&required_registry_api, &current_registry_api) {
            return Err(ProcedureLoadError::IncompatibleRegistryApi {
                name: definition.name,
                required: required_registry_api.to_string(),
                current: current_registry_api.to_string(),
            });
        }

        self.register(procedure);
        Ok(())
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

fn is_semver_compatible(required: &Version, current: &Version) -> bool {
    if required.major == 0 {
        required.major == current.major && required.minor == current.minor && current >= required
    } else {
        required.major == current.major && current >= required
    }
}

/// Generate a stable plugin template for implementing a versioned procedure.
pub fn plugin_template_generator(plugin_name: &str, event_type: &str) -> String {
    let sanitized_name = plugin_name.trim();
    let sanitized_event = event_type.trim();
    let definition = ProcedureDefinition::new(sanitized_name, sanitized_event);
    format!(
        r#"use async_trait::async_trait;
use pares_agens_core::{{event::Event, procedure::{{Procedure, ProcedureDefinition}}}};

pub struct {struct_name};

pub fn definition() -> ProcedureDefinition {{
    ProcedureDefinition {{
        name: "{name}".to_string(),
        event_type: "{event_type}".to_string(),
        version: "{version}".to_string(),
        registry_api_version: "{api_version}".to_string(),
    }}
}}

#[async_trait]
impl Procedure for {struct_name} {{
    fn name(&self) -> &str {{ "{name}" }}

    fn handles(&self) -> &str {{ "{event_type}" }}

    async fn execute(&self, _event: &Event) -> Vec<Event> {{
        vec![]
    }}
}}
"#,
        struct_name = to_pascal_case(sanitized_name),
        name = definition.name,
        event_type = definition.event_type,
        version = definition.version,
        api_version = definition.registry_api_version
    )
}

fn to_pascal_case(input: &str) -> String {
    let mut out = String::new();
    for segment in input
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        "PluginProcedure".to_string()
    } else if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("Plugin{}", out)
    } else {
        out
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

    #[test]
    fn procedure_definition_new_defaults_to_semver_and_registry_api() {
        let def = ProcedureDefinition::new("p1", "message");
        assert_eq!(def.version, "1.0.0");
        assert_eq!(def.registry_api_version, PROCEDURE_REGISTRY_API_VERSION);
    }

    #[test]
    fn registry_exposes_stable_api_version() {
        let registry = ProcedureRegistry::new();
        assert_eq!(registry.api_version(), PROCEDURE_REGISTRY_API_VERSION);
    }

    #[test]
    fn load_definition_registers_when_semver_is_compatible() {
        let mut registry = ProcedureRegistry::new();
        let def = ProcedureDefinition::new("p1", "message");

        registry
            .load_definition(
                def,
                Box::new(Noop {
                    name: "p1",
                    handles: "message",
                }),
            )
            .unwrap();

        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn load_definition_rejects_invalid_procedure_semver() {
        let mut registry = ProcedureRegistry::new();
        let mut def = ProcedureDefinition::new("p1", "message");
        def.version = "not-a-version".to_string();

        let err = registry
            .load_definition(
                def,
                Box::new(Noop {
                    name: "p1",
                    handles: "message",
                }),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            ProcedureLoadError::InvalidProcedureVersion { .. }
        ));
    }

    #[test]
    fn load_definition_rejects_incompatible_registry_api() {
        let mut registry = ProcedureRegistry::new();
        let mut def = ProcedureDefinition::new("p1", "message");
        def.registry_api_version = "2.0.0".to_string();

        let err = registry
            .load_definition(
                def,
                Box::new(Noop {
                    name: "p1",
                    handles: "message",
                }),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            ProcedureLoadError::IncompatibleRegistryApi { .. }
        ));
    }

    #[test]
    fn plugin_template_generator_embeds_definition_and_trait_impl() {
        let template = plugin_template_generator("hello_plugin", "message");
        assert!(template.contains("pub fn definition() -> ProcedureDefinition"));
        assert!(template.contains("name: \"hello_plugin\".to_string()"));
        assert!(template.contains("event_type: \"message\".to_string()"));
        assert!(template.contains("registry_api_version: \"1.0.0\".to_string()"));
        assert!(template.contains("impl Procedure for HelloPlugin"));
    }
}
