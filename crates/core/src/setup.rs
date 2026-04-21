//! First-run wizard configuration and state persistence.
//!
//! [`SetupWizard`] guides the user through naming the agent, picking a model
//! provider, optionally connecting Telegram, and then persisting the resulting
//! [`SetupConfig`] to the PluresDB [`StateStore`].
//!
//! # Wizard flow
//!
//! 1. [`WizardStep::AgentName`]       — enter a display name for the agent.
//! 2. [`WizardStep::ModelPicker`]     — choose a model backend.
//! 3. [`WizardStep::TelegramConnect`] — optionally supply a Telegram bot token.
//! 4. [`WizardStep::Done`]            — config is persisted; drop into chat.

use std::collections::{BTreeSet, HashMap, VecDeque};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::state::StateStore;

/// PluresDB key under which the setup config is stored.
pub const SETUP_CONFIG_KEY: &str = "agent.setup_config";
/// PluresDB key under which setup feature schemas are stored.
pub const SETUP_SCHEMAS_KEY: &str = "agent.setup.schemas";

// ---------------------------------------------------------------------------
// Model choice
// ---------------------------------------------------------------------------

/// Which model backend the agent should use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelChoice {
    /// Docker Model Runner running locally (no API key required).
    DockerModelRunner {
        /// HTTP base URL, e.g. `http://localhost:12434`.
        base_url: String,
    },
    /// Remote provider accessed via an API key.
    ApiKey {
        /// Provider name, e.g. `"openai"` or `"anthropic"`.
        provider: String,
        /// HTTP base URL of the OpenAI-compatible endpoint.
        base_url: String,
        /// Bearer API key.
        api_key: String,
    },
}

// ---------------------------------------------------------------------------
// Telegram setup
// ---------------------------------------------------------------------------

/// Optional Telegram connection details collected during setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramSetup {
    /// Bot token from BotFather.
    pub token: String,
}

// ---------------------------------------------------------------------------
// SetupConfig
// ---------------------------------------------------------------------------

/// Full configuration produced by the first-run wizard.
///
/// Persisted under [`SETUP_CONFIG_KEY`] in the PluresDB state store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupConfig {
    /// Display name for the agent (step 1 of the wizard).
    pub agent_name: String,
    /// Chosen model backend (step 2 of the wizard).
    pub model: ModelChoice,
    /// Optional Telegram connection (step 3 of the wizard).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramSetup>,
    /// Whether setup has been completed.
    pub setup_complete: bool,
}

// ---------------------------------------------------------------------------
// WizardStep
// ---------------------------------------------------------------------------

/// Wizard step progression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WizardStep {
    /// Step 1: choose an agent name.
    AgentName,
    /// Step 2: pick a model backend.
    ModelPicker,
    /// Step 3: optionally connect Telegram.
    TelegramConnect,
    /// Setup complete — ready to chat.
    Done,
}

impl WizardStep {
    /// Human-readable label for the current step.
    pub fn label(&self) -> &'static str {
        match self {
            WizardStep::AgentName => "Name your agent",
            WizardStep::ModelPicker => "Pick a model",
            WizardStep::TelegramConnect => "Connect Telegram (optional)",
            WizardStep::Done => "Done",
        }
    }

    /// Advance to the next step.
    pub fn next(&self) -> Self {
        match self {
            WizardStep::AgentName => WizardStep::ModelPicker,
            WizardStep::ModelPicker => WizardStep::TelegramConnect,
            WizardStep::TelegramConnect => WizardStep::Done,
            WizardStep::Done => WizardStep::Done,
        }
    }
}

// ---------------------------------------------------------------------------
// SetupWizard
// ---------------------------------------------------------------------------

/// First-run setup wizard.
///
/// Tracks the current step and accumulates configuration as the user progresses
/// through the wizard.  Call [`SetupWizard::save`] to persist the final
/// [`SetupConfig`] to PluresDB when the wizard completes.
#[derive(Debug)]
pub struct SetupWizard {
    /// The current wizard step.
    pub step: WizardStep,
    /// Agent name collected in step 1.
    pub agent_name: Option<String>,
    /// Model choice collected in step 2.
    pub model: Option<ModelChoice>,
    /// Optional Telegram setup collected in step 3.
    pub telegram: Option<TelegramSetup>,
}

impl SetupWizard {
    /// Create a new wizard starting at the first step.
    pub fn new() -> Self {
        Self {
            step: WizardStep::AgentName,
            agent_name: None,
            model: None,
            telegram: None,
        }
    }

    /// Set the agent name and advance to the next step.
    ///
    /// Returns an error if called when the wizard is not in the `AgentName` step.
    pub fn set_agent_name(&mut self, name: impl Into<String>) -> Result<(), String> {
        match self.step {
            WizardStep::AgentName => {
                self.agent_name = Some(name.into());
                self.step = self.step.next();
                Ok(())
            }
            _ => Err(format!(
                "cannot set agent name while in {:?} step",
                self.step
            )),
        }
    }

    /// Set the model choice and advance to the next step.
    ///
    /// Returns an error if called when the wizard is not in the `ModelPicker` step.
    pub fn set_model(&mut self, model: ModelChoice) -> Result<(), String> {
        match self.step {
            WizardStep::ModelPicker => {
                self.model = Some(model);
                self.step = self.step.next();
                Ok(())
            }
            _ => Err(format!("cannot set model while in {:?} step", self.step)),
        }
    }

    /// Set optional Telegram credentials and advance to the next step.
    ///
    /// Returns an error if called when the wizard is not in the `TelegramConnect` step.
    pub fn set_telegram(&mut self, setup: Option<TelegramSetup>) -> Result<(), String> {
        match self.step {
            WizardStep::TelegramConnect => {
                self.telegram = setup;
                self.step = self.step.next();
                Ok(())
            }
            _ => Err(format!(
                "cannot set Telegram setup while in {:?} step",
                self.step
            )),
        }
    }

    /// Whether the wizard has collected all required fields.
    pub fn is_complete(&self) -> bool {
        self.step == WizardStep::Done && self.agent_name.is_some() && self.model.is_some()
    }

    /// Build the final [`SetupConfig`].
    ///
    /// Returns `None` if required fields are missing.
    pub fn build(&self) -> Option<SetupConfig> {
        Some(SetupConfig {
            agent_name: self.agent_name.clone()?,
            model: self.model.clone()?,
            telegram: self.telegram.clone(),
            setup_complete: true,
        })
    }

    /// Persist the completed config to the given state store.
    ///
    /// Returns `Err` if required fields are missing or serialization fails.
    pub async fn save(&self, store: &dyn StateStore) -> Result<SetupConfig, String> {
        let config = self
            .build()
            .ok_or_else(|| "wizard is not complete".to_string())?;
        let value = serde_json::to_value(&config).map_err(|e| e.to_string())?;
        store.set(SETUP_CONFIG_KEY, value).await;
        Ok(config)
    }

    /// Load an existing config from the state store (if present).
    pub async fn load(store: &dyn StateStore) -> Option<SetupConfig> {
        let value = store.get(SETUP_CONFIG_KEY).await?;
        serde_json::from_value(value).ok()
    }
}

impl Default for SetupWizard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Generic setup schema engine
// ---------------------------------------------------------------------------

/// Supported setting value kinds for setup schemas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupSettingType {
    /// Plain text value.
    Text,
    /// Secret text value (tokens/keys/passwords).
    Secret,
    /// Numeric value.
    Number,
    /// Boolean value.
    Boolean,
}

/// One user-provided setting required by a setup feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSettingSchema {
    /// Stable setting key persisted in PluresDB.
    pub key: String,
    /// Setting value type.
    #[serde(rename = "type")]
    pub setting_type: SetupSettingType,
    /// User-facing prompt.
    pub prompt: String,
    /// Optional validation rule identifier.
    #[serde(default)]
    pub validate: Option<String>,
    /// Whether this setting may be omitted.
    #[serde(default)]
    pub optional: bool,
}

/// Setup schema for a single feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupFeatureSchema {
    /// Stable feature identifier (e.g. `"telegram"`).
    pub id: String,
    /// Human-readable feature name.
    pub name: String,
    /// Features that must be configured first.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Settings required to configure this feature.
    #[serde(default)]
    pub settings: Vec<SetupSettingSchema>,
    /// Praxis constraints used by this feature.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Action to execute after successful setup.
    #[serde(rename = "onComplete", default)]
    pub on_complete: Option<String>,
}

/// Runtime summary describing one pending setting prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPrompt {
    /// Feature that owns this setting.
    pub feature_id: String,
    /// Feature display name.
    pub feature_name: String,
    /// Setting schema.
    pub setting: SetupSettingSchema,
}

/// Hook for executing feature `onComplete` actions.
#[async_trait]
pub trait SetupActionExecutor: Send + Sync {
    /// Execute a named setup completion action.
    async fn execute(
        &self,
        action: &str,
        feature_id: &str,
        store: &dyn StateStore,
    ) -> Result<(), String>;
}

/// No-op completion action executor.
pub struct NoopSetupActionExecutor;

#[async_trait]
impl SetupActionExecutor for NoopSetupActionExecutor {
    async fn execute(
        &self,
        _action: &str,
        _feature_id: &str,
        _store: &dyn StateStore,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Generic setup wizard engine driven by feature schemas.
#[derive(Debug, Clone)]
pub struct SetupSchemaEngine {
    schemas: Vec<SetupFeatureSchema>,
}

impl SetupSchemaEngine {
    /// Build an engine from pre-loaded schemas.
    pub fn new(schemas: Vec<SetupFeatureSchema>) -> Result<Self, String> {
        let mut seen = BTreeSet::new();
        for schema in &schemas {
            if schema.id.trim().is_empty() {
                return Err("setup schema id cannot be empty".to_string());
            }
            if !seen.insert(schema.id.clone()) {
                return Err(format!("duplicate setup schema id: {}", schema.id));
            }
        }
        Ok(Self { schemas })
    }

    /// Discover setup schemas from PluresDB.
    pub async fn discover(store: &dyn StateStore) -> Result<Self, String> {
        match store.get(SETUP_SCHEMAS_KEY).await {
            None => Self::new(Vec::new()),
            Some(value) => {
                let schemas: Vec<SetupFeatureSchema> =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                Self::new(schemas)
            }
        }
    }

    /// Persist setup schemas to PluresDB.
    pub async fn persist(&self, store: &dyn StateStore) -> Result<(), String> {
        let value = serde_json::to_value(&self.schemas).map_err(|e| e.to_string())?;
        store.set(SETUP_SCHEMAS_KEY, value).await;
        Ok(())
    }

    /// Read-only access to all discovered schemas.
    pub fn schemas(&self) -> &[SetupFeatureSchema] {
        &self.schemas
    }

    /// Find a feature schema by id.
    pub fn schema(&self, feature_id: &str) -> Option<&SetupFeatureSchema> {
        self.schemas.iter().find(|schema| schema.id == feature_id)
    }

    /// Resolve setup order based on `requires` dependencies.
    pub fn resolve_dependency_order(&self) -> Result<Vec<&SetupFeatureSchema>, String> {
        let id_to_schema: HashMap<&str, &SetupFeatureSchema> = self
            .schemas
            .iter()
            .map(|schema| (schema.id.as_str(), schema))
            .collect();
        let mut indegree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for schema in &self.schemas {
            indegree.insert(schema.id.as_str(), schema.requires.len());
            for dependency in &schema.requires {
                let dep = dependency.as_str();
                if !id_to_schema.contains_key(dep) {
                    return Err(format!(
                        "feature '{}' depends on unknown feature '{}'",
                        schema.id, dependency
                    ));
                }
                dependents.entry(dep).or_default().push(schema.id.as_str());
            }
        }

        let mut ready: Vec<&str> = indegree
            .iter()
            .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
            .collect();
        ready.sort_unstable();
        let mut queue: VecDeque<&str> = ready.into();
        let mut ordered_ids = Vec::with_capacity(self.schemas.len());

        while let Some(id) = queue.pop_front() {
            ordered_ids.push(id);
            if let Some(children) = dependents.get(id) {
                let mut newly_ready = Vec::new();
                for child in children {
                    if let Some(entry) = indegree.get_mut(child) {
                        *entry = entry.saturating_sub(1);
                        if *entry == 0 {
                            newly_ready.push(*child);
                        }
                    }
                }
                newly_ready.sort_unstable();
                for child in newly_ready {
                    queue.push_back(child);
                }
            }
        }

        if ordered_ids.len() != self.schemas.len() {
            return Err("setup schema dependency cycle detected".to_string());
        }

        Ok(ordered_ids
            .into_iter()
            .filter_map(|id| id_to_schema.get(id).copied())
            .collect())
    }

    /// Resolve features that still require setup, in dependency order.
    pub async fn pending_features(
        &self,
        store: &dyn StateStore,
    ) -> Result<Vec<&SetupFeatureSchema>, String> {
        let mut pending = Vec::new();
        for schema in self.resolve_dependency_order()? {
            if !self.is_configured(store, schema).await? {
                pending.push(schema);
            }
        }
        Ok(pending)
    }

    /// Flatten pending features to ordered prompts for user-facing setup surfaces.
    pub async fn pending_prompts(
        &self,
        store: &dyn StateStore,
    ) -> Result<Vec<SetupPrompt>, String> {
        let mut prompts = Vec::new();
        for feature in self.pending_features(store).await? {
            for setting in &feature.settings {
                prompts.push(SetupPrompt {
                    feature_id: feature.id.clone(),
                    feature_name: feature.name.clone(),
                    setting: setting.clone(),
                });
            }
        }
        Ok(prompts)
    }

    /// Validate and persist one feature's setup values.
    ///
    /// Returns the feature's optional `onComplete` action.
    pub async fn store_feature_settings(
        &self,
        store: &dyn StateStore,
        feature_id: &str,
        provided: &Map<String, Value>,
    ) -> Result<Option<String>, String> {
        let schema = self
            .schema(feature_id)
            .ok_or_else(|| format!("unknown setup feature: {feature_id}"))?;

        for setting in &schema.settings {
            let Some(value) = provided.get(&setting.key) else {
                if setting.optional {
                    continue;
                }
                return Err(format!(
                    "missing setting '{}' for feature '{}'",
                    setting.key, feature_id
                ));
            };

            let text_value = coerce_setting_text(value).map_err(|err| {
                format!(
                    "invalid setting '{}' for feature '{}': {err}",
                    setting.key, feature_id
                )
            })?;
            validate_setting(setting, &text_value).map_err(|err| {
                format!(
                    "setting '{}' for feature '{}' failed validation: {err}",
                    setting.key, feature_id
                )
            })?;
        }

        store
            .set(
                &setup_feature_settings_key(feature_id),
                Value::Object(provided.clone()),
            )
            .await;
        store
            .set(&setup_feature_configured_key(feature_id), Value::Bool(true))
            .await;
        Ok(schema.on_complete.clone())
    }

    /// Validate, persist, and execute feature setup completion action.
    pub async fn complete_feature(
        &self,
        store: &dyn StateStore,
        feature_id: &str,
        provided: &Map<String, Value>,
        executor: &dyn SetupActionExecutor,
    ) -> Result<(), String> {
        if let Some(action) = self
            .store_feature_settings(store, feature_id, provided)
            .await?
            .map(|action| action.trim().to_string())
            .filter(|action| !action.is_empty())
        {
            executor.execute(&action, feature_id, store).await?;
        }
        Ok(())
    }

    async fn is_configured(
        &self,
        store: &dyn StateStore,
        schema: &SetupFeatureSchema,
    ) -> Result<bool, String> {
        if !schema.settings.iter().any(|setting| !setting.optional) {
            return Ok(true);
        }

        if let Some(Value::Bool(true)) = store.get(&setup_feature_configured_key(&schema.id)).await
        {
            return Ok(true);
        }

        let Some(Value::Object(values)) = store.get(&setup_feature_settings_key(&schema.id)).await
        else {
            return Ok(false);
        };
        for setting in &schema.settings {
            if setting.optional {
                continue;
            }
            let Some(value) = values.get(&setting.key) else {
                return Ok(false);
            };
            let text = coerce_setting_text(value)?;
            if text.trim().is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Build the PluresDB key used to persist a feature's settings map.
pub fn setup_feature_settings_key(feature_id: &str) -> String {
    format!("agent.setup.feature.{feature_id}.settings")
}

/// Build the PluresDB key used to persist a feature's configured marker.
pub fn setup_feature_configured_key(feature_id: &str) -> String {
    format!("agent.setup.feature.{feature_id}.configured")
}

fn coerce_setting_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(inner) => Ok(inner.clone()),
        Value::Number(inner) => Ok(inner.to_string()),
        Value::Bool(inner) => Ok(inner.to_string()),
        _ => Err("must be a string, number, or boolean".to_string()),
    }
}

fn validate_setting(setting: &SetupSettingSchema, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    match setting.setting_type {
        SetupSettingType::Text | SetupSettingType::Secret => {
            if !setting.optional && trimmed.is_empty() {
                return Err("value cannot be empty".to_string());
            }
        }
        SetupSettingType::Number => {
            if !setting.optional && trimmed.is_empty() {
                return Err("value cannot be empty".to_string());
            }
            if !trimmed.is_empty() && trimmed.parse::<f64>().is_err() {
                return Err("expected a number".to_string());
            }
        }
        SetupSettingType::Boolean => {
            if !trimmed.eq_ignore_ascii_case("true") && !trimmed.eq_ignore_ascii_case("false") {
                return Err("expected true or false".to_string());
            }
        }
    }

    if let Some(rule) = setting.validate.as_deref() {
        apply_validation_rule(rule, trimmed)?;
    }
    Ok(())
}

fn apply_validation_rule(rule: &str, value: &str) -> Result<(), String> {
    if let Some(min_len) = rule.strip_prefix("min_len:") {
        let min_len: usize = min_len
            .parse()
            .map_err(|_| format!("invalid validation rule: {rule}"))?;
        if value.chars().count() >= min_len {
            return Ok(());
        }
        return Err(format!("value must be at least {min_len} characters"));
    }

    match rule {
        "non_empty" => {
            if value.is_empty() {
                Err("value cannot be empty".to_string())
            } else {
                Ok(())
            }
        }
        "starts_with_digit" => {
            if value.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                Ok(())
            } else {
                Err("value must start with a digit".to_string())
            }
        }
        "starts_with_at" => {
            if value.starts_with('@') {
                Ok(())
            } else {
                Err("value must start with '@'".to_string())
            }
        }
        _ => Err(format!("unknown validation rule: {rule}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    struct MockStore(Mutex<HashMap<String, serde_json::Value>>);

    impl MockStore {
        fn new() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    #[async_trait::async_trait]
    impl StateStore for MockStore {
        async fn get(&self, key: &str) -> Option<serde_json::Value> {
            self.0.lock().unwrap().get(key).cloned()
        }

        async fn set(&self, key: &str, value: serde_json::Value) {
            self.0.lock().unwrap().insert(key.to_string(), value);
        }
    }

    fn docker_model() -> ModelChoice {
        ModelChoice::DockerModelRunner {
            base_url: "http://localhost:12434".into(),
        }
    }

    #[test]
    fn wizard_starts_at_agent_name_step() {
        let wizard = SetupWizard::new();
        assert_eq!(wizard.step, WizardStep::AgentName);
    }

    #[test]
    fn wizard_step_progression() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria").unwrap();
        assert_eq!(wizard.step, WizardStep::ModelPicker);
        wizard.set_model(docker_model()).unwrap();
        assert_eq!(wizard.step, WizardStep::TelegramConnect);
        wizard.set_telegram(None).unwrap();
        assert_eq!(wizard.step, WizardStep::Done);
        assert!(wizard.is_complete());
    }

    #[test]
    fn wizard_is_not_complete_before_all_steps() {
        let mut wizard = SetupWizard::new();
        assert!(!wizard.is_complete());
        wizard.set_agent_name("Aria").unwrap();
        assert!(!wizard.is_complete());
        wizard.set_model(docker_model()).unwrap();
        assert!(!wizard.is_complete());
    }

    #[test]
    fn wizard_build_returns_config() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria").unwrap();
        wizard.set_model(docker_model()).unwrap();
        wizard.set_telegram(None).unwrap();
        let config = wizard.build().unwrap();
        assert_eq!(config.agent_name, "Aria");
        assert!(config.setup_complete);
        assert!(config.telegram.is_none());
    }

    #[test]
    fn wizard_build_returns_none_when_incomplete() {
        let wizard = SetupWizard::new();
        assert!(wizard.build().is_none());
    }

    #[tokio::test]
    async fn wizard_saves_and_loads_from_store() {
        let store = MockStore::new();
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria").unwrap();
        wizard.set_model(docker_model()).unwrap();
        wizard.set_telegram(None).unwrap();

        let saved = wizard.save(&store).await.unwrap();
        assert_eq!(saved.agent_name, "Aria");

        let loaded = SetupWizard::load(&store).await.unwrap();
        assert_eq!(loaded, saved);
    }

    #[tokio::test]
    async fn wizard_save_fails_when_incomplete() {
        let store = MockStore::new();
        let wizard = SetupWizard::new();
        assert!(wizard.save(&store).await.is_err());
    }

    #[tokio::test]
    async fn wizard_load_returns_none_when_not_set() {
        let store = MockStore::new();
        let result = SetupWizard::load(&store).await;
        assert!(result.is_none());
    }

    #[test]
    fn wizard_step_labels() {
        assert_eq!(WizardStep::AgentName.label(), "Name your agent");
        assert_eq!(WizardStep::ModelPicker.label(), "Pick a model");
        assert_eq!(
            WizardStep::TelegramConnect.label(),
            "Connect Telegram (optional)"
        );
        assert_eq!(WizardStep::Done.label(), "Done");
    }

    #[test]
    fn wizard_done_step_does_not_advance() {
        let step = WizardStep::Done;
        assert_eq!(step.next(), WizardStep::Done);
    }

    #[test]
    fn wizard_with_telegram() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria").unwrap();
        wizard.set_model(docker_model()).unwrap();
        wizard
            .set_telegram(Some(TelegramSetup {
                token: "tok".into(),
            }))
            .unwrap();

        let config = wizard.build().unwrap();
        assert!(config.telegram.is_some());
        assert_eq!(config.telegram.unwrap().token, "tok");
    }

    #[test]
    fn wizard_with_api_key_model() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria").unwrap();
        wizard
            .set_model(ModelChoice::ApiKey {
                provider: "openai".into(),
                base_url: "https://api.openai.com/v1".into(),
                api_key: "sk-test".into(),
            })
            .unwrap();
        wizard.set_telegram(None).unwrap();

        let config = wizard.build().unwrap();
        assert!(matches!(config.model, ModelChoice::ApiKey { .. }));
    }

    #[test]
    fn setup_config_serializes_without_telegram() {
        let config = SetupConfig {
            agent_name: "Aria".into(),
            model: docker_model(),
            telegram: None,
            setup_complete: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("telegram"),
            "null telegram should be omitted"
        );
    }

    #[test]
    fn wizard_setters_reject_out_of_order_calls() {
        let mut wizard = SetupWizard::new();
        // Calling set_model before set_agent_name must fail.
        assert!(wizard.set_model(docker_model()).is_err());
        // Calling set_telegram before completing earlier steps must also fail.
        assert!(wizard.set_telegram(None).is_err());
        // Correct order succeeds.
        wizard.set_agent_name("Aria").unwrap();
        // Now set_agent_name again must fail.
        assert!(wizard.set_agent_name("Bob").is_err());
    }

    fn telegram_schema() -> SetupFeatureSchema {
        SetupFeatureSchema {
            id: "telegram".to_string(),
            name: "Telegram Bot".to_string(),
            requires: vec![],
            settings: vec![SetupSettingSchema {
                key: "telegram_token".to_string(),
                setting_type: SetupSettingType::Secret,
                prompt: "Bot token from @BotFather".to_string(),
                validate: Some("starts_with_digit".to_string()),
                optional: false,
            }],
            constraints: vec!["C-ADAPTER-001".to_string()],
            on_complete: Some("restart_adapter".to_string()),
        }
    }

    fn hyperswarm_schema() -> SetupFeatureSchema {
        SetupFeatureSchema {
            id: "hyperswarm".to_string(),
            name: "Hyperswarm".to_string(),
            requires: vec!["telegram".to_string()],
            settings: vec![SetupSettingSchema {
                key: "topic".to_string(),
                setting_type: SetupSettingType::Text,
                prompt: "Sync topic".to_string(),
                validate: Some("min_len:8".to_string()),
                optional: false,
            }],
            constraints: vec![],
            on_complete: None,
        }
    }

    fn no_settings_schema() -> SetupFeatureSchema {
        SetupFeatureSchema {
            id: "telemetry".to_string(),
            name: "Telemetry".to_string(),
            requires: vec![],
            settings: vec![],
            constraints: vec![],
            on_complete: Some("enable_telemetry".to_string()),
        }
    }

    struct RecordingActionExecutor {
        calls: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl SetupActionExecutor for RecordingActionExecutor {
        async fn execute(
            &self,
            action: &str,
            feature_id: &str,
            _store: &dyn StateStore,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((action.to_string(), feature_id.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn schema_engine_persist_and_discover_roundtrip() {
        let store = MockStore::new();
        let engine = SetupSchemaEngine::new(vec![telegram_schema(), hyperswarm_schema()]).unwrap();
        engine.persist(&store).await.unwrap();

        let loaded = SetupSchemaEngine::discover(&store).await.unwrap();
        assert_eq!(loaded.schemas(), engine.schemas());
    }

    #[test]
    fn schema_engine_orders_dependencies() {
        let engine = SetupSchemaEngine::new(vec![hyperswarm_schema(), telegram_schema()]).unwrap();
        let ordered = engine.resolve_dependency_order().unwrap();
        let ids = ordered
            .iter()
            .map(|schema| schema.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["telegram", "hyperswarm"]);
    }

    #[test]
    fn schema_engine_rejects_unknown_dependencies() {
        let schema = SetupFeatureSchema {
            id: "x".to_string(),
            name: "X".to_string(),
            requires: vec!["missing".to_string()],
            settings: vec![],
            constraints: vec![],
            on_complete: None,
        };
        let engine = SetupSchemaEngine::new(vec![schema]).unwrap();
        assert!(engine.resolve_dependency_order().is_err());
    }

    #[tokio::test]
    async fn schema_engine_pending_features_skip_configured() {
        let store = MockStore::new();
        store
            .set(&setup_feature_configured_key("telegram"), Value::Bool(true))
            .await;

        let engine = SetupSchemaEngine::new(vec![telegram_schema(), hyperswarm_schema()]).unwrap();
        let pending = engine.pending_features(&store).await.unwrap();
        let ids = pending
            .iter()
            .map(|schema| schema.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["hyperswarm"]);
    }

    #[tokio::test]
    async fn schema_engine_treats_no_settings_feature_as_configured() {
        let store = MockStore::new();
        let engine = SetupSchemaEngine::new(vec![no_settings_schema()]).unwrap();
        let pending = engine.pending_features(&store).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn schema_engine_complete_feature_persists_and_executes_action() {
        let store = MockStore::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let executor = RecordingActionExecutor {
            calls: Arc::clone(&calls),
        };
        let engine = SetupSchemaEngine::new(vec![telegram_schema()]).unwrap();
        let provided = Map::from_iter([("telegram_token".to_string(), json!("123:token"))]);

        engine
            .complete_feature(&store, "telegram", &provided, &executor)
            .await
            .unwrap();

        let configured = store
            .get(&setup_feature_configured_key("telegram"))
            .await
            .unwrap();
        assert_eq!(configured, Value::Bool(true));

        let settings = store
            .get(&setup_feature_settings_key("telegram"))
            .await
            .unwrap();
        assert_eq!(settings, Value::Object(provided));

        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.as_slice(),
            &[("restart_adapter".to_string(), "telegram".to_string())]
        );
    }

    #[tokio::test]
    async fn schema_engine_complete_feature_rejects_invalid_value() {
        let store = MockStore::new();
        let engine = SetupSchemaEngine::new(vec![telegram_schema()]).unwrap();
        let provided = Map::from_iter([("telegram_token".to_string(), json!("token"))]);

        let err = engine
            .store_feature_settings(&store, "telegram", &provided)
            .await
            .unwrap_err();
        assert!(err.contains("failed validation"));
    }
}
