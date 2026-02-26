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

use serde::{Deserialize, Serialize};

use crate::state::StateStore;

/// PluresDB key under which the setup config is stored.
pub const SETUP_CONFIG_KEY: &str = "agent.setup_config";

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
            _ => Err(format!(
                "cannot set model while in {:?} step",
                self.step
            )),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

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
        wizard.set_agent_name("Aria");
        assert_eq!(wizard.step, WizardStep::ModelPicker);
        wizard.set_model(docker_model());
        assert_eq!(wizard.step, WizardStep::TelegramConnect);
        wizard.set_telegram(None);
        assert_eq!(wizard.step, WizardStep::Done);
        assert!(wizard.is_complete());
    }

    #[test]
    fn wizard_is_not_complete_before_all_steps() {
        let mut wizard = SetupWizard::new();
        assert!(!wizard.is_complete());
        wizard.set_agent_name("Aria");
        assert!(!wizard.is_complete());
        wizard.set_model(docker_model());
        assert!(!wizard.is_complete());
    }

    #[test]
    fn wizard_build_returns_config() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria");
        wizard.set_model(docker_model());
        wizard.set_telegram(None);
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
        wizard.set_agent_name("Aria");
        wizard.set_model(docker_model());
        wizard.set_telegram(None);

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
        assert_eq!(WizardStep::TelegramConnect.label(), "Connect Telegram (optional)");
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
        wizard.set_agent_name("Aria");
        wizard.set_model(docker_model());
        wizard.set_telegram(Some(TelegramSetup { token: "tok".into() }));

        let config = wizard.build().unwrap();
        assert!(config.telegram.is_some());
        assert_eq!(config.telegram.unwrap().token, "tok");
    }

    #[test]
    fn wizard_with_api_key_model() {
        let mut wizard = SetupWizard::new();
        wizard.set_agent_name("Aria");
        wizard.set_model(ModelChoice::ApiKey {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-test".into(),
        });
        wizard.set_telegram(None);

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
        assert!(!json.contains("telegram"), "null telegram should be omitted");
    }
}
