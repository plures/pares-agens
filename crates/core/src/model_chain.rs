//! Model selection chain — tries cheap first, escalates on need.
//!
//! The [`ModelChain`] holds optional references to three model tiers (BitNet
//! local, cloud conscious, cloud deep) and selects the best available model
//! based on the cerebellum's [`MessageClassification`].

use crate::cerebellum::classifier::MessageClassification;
use pares_radix_core::model::ModelClient;
use std::sync::Arc;

/// Model selection chain — tries cheap first, escalates on need.
pub struct ModelChain {
    /// Local BitNet (classifier + light generation).
    pub bitnet: Option<Arc<dyn ModelClient>>,
    /// Cloud conscious model (GPT-4.1, Sonnet).
    pub conscious: Option<Arc<dyn ModelClient>>,
    /// Cloud deep model (GPT-5.2, Opus).
    pub deep: Option<Arc<dyn ModelClient>>,
}

impl ModelChain {
    /// Create a new model chain with the given tiers.
    pub fn new(
        bitnet: Option<Arc<dyn ModelClient>>,
        conscious: Option<Arc<dyn ModelClient>>,
        deep: Option<Arc<dyn ModelClient>>,
    ) -> Self {
        Self {
            bitnet,
            conscious,
            deep,
        }
    }

    /// Select the best available model for a given classification.
    pub fn select(&self, classification: &MessageClassification) -> ModelSelection {
        // If deep model needed AND available → use deep
        if classification.needs_deep_model {
            if let Some(deep) = &self.deep {
                return ModelSelection::Deep(deep.clone());
            }
            // Fall through to conscious or bitnet
        }

        // If cloud conscious available → use it
        if let Some(conscious) = &self.conscious {
            return ModelSelection::Conscious(conscious.clone());
        }

        // If only BitNet available (offline/airgapped) → use it for everything
        if let Some(bitnet) = &self.bitnet {
            return ModelSelection::BitNetFull(bitnet.clone());
        }

        ModelSelection::None
    }

    /// Is this an airgapped/offline deployment?
    pub fn is_offline(&self) -> bool {
        self.conscious.is_none() && self.deep.is_none()
    }

    /// What models are available?
    pub fn status(&self) -> ModelChainStatus {
        ModelChainStatus {
            bitnet: self.bitnet.is_some(),
            conscious: self.conscious.is_some(),
            deep: self.deep.is_some(),
            mode: if self.conscious.is_some() {
                "connected".to_string()
            } else if self.bitnet.is_some() {
                "offline (BitNet)".to_string()
            } else {
                "no models".to_string()
            },
        }
    }
}

/// The selected model tier and client.
pub enum ModelSelection {
    /// Deep reasoning model (GPT-5.2, Opus).
    Deep(Arc<dyn ModelClient>),
    /// Cloud conscious model (GPT-4.1, Sonnet).
    Conscious(Arc<dyn ModelClient>),
    /// BitNet handling all roles (offline mode).
    BitNetFull(Arc<dyn ModelClient>),
    /// No models available.
    None,
}

impl ModelSelection {
    /// Returns `true` if no model is available.
    pub fn is_none(&self) -> bool {
        matches!(self, ModelSelection::None)
    }

    /// Returns the model client if one was selected.
    pub fn client(&self) -> Option<&Arc<dyn ModelClient>> {
        match self {
            ModelSelection::Deep(c)
            | ModelSelection::Conscious(c)
            | ModelSelection::BitNetFull(c) => Some(c),
            ModelSelection::None => None,
        }
    }

    /// Human-readable label for the selected tier.
    pub fn tier_label(&self) -> &'static str {
        match self {
            ModelSelection::Deep(_) => "deep",
            ModelSelection::Conscious(_) => "conscious",
            ModelSelection::BitNetFull(_) => "bitnet",
            ModelSelection::None => "none",
        }
    }
}

/// Status snapshot of the model chain.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelChainStatus {
    pub bitnet: bool,
    pub conscious: bool,
    pub deep: bool,
    pub mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cerebellum::classifier::{MessageClassification, MessageIntent};
    use pares_radix_core::model::{ChatMessage, ChatOptions, ModelCompletion, ToolDefinition};
    use async_trait::async_trait;

    struct MockClient(&'static str);

    #[async_trait]
    impl ModelClient for MockClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some(self.0.to_string()),
                tool_calls: vec![],
                logprobs: None,
                model: None,
            })
        }
    }

    fn classification(needs_deep: bool) -> MessageClassification {
        MessageClassification {
            intent: MessageIntent::Task,
            complexity: if needs_deep { 5 } else { 2 },
            topic: "test".into(),
            topic_shift: false,
            entities: vec![],
            plugin_match: None,
            completion_hint: None,
            needs_tools: false,
            needs_deep_model: needs_deep,
        }
    }

    #[test]
    fn selects_deep_when_needed_and_available() {
        let chain = ModelChain::new(
            None,
            Some(Arc::new(MockClient("conscious"))),
            Some(Arc::new(MockClient("deep"))),
        );
        let sel = chain.select(&classification(true));
        assert_eq!(sel.tier_label(), "deep");
    }

    #[test]
    fn falls_through_to_conscious_when_deep_unavailable() {
        let chain = ModelChain::new(None, Some(Arc::new(MockClient("conscious"))), None);
        let sel = chain.select(&classification(true));
        assert_eq!(sel.tier_label(), "conscious");
    }

    #[test]
    fn selects_conscious_for_simple_tasks() {
        let chain = ModelChain::new(
            Some(Arc::new(MockClient("bitnet"))),
            Some(Arc::new(MockClient("conscious"))),
            None,
        );
        let sel = chain.select(&classification(false));
        assert_eq!(sel.tier_label(), "conscious");
    }

    #[test]
    fn offline_mode_uses_bitnet() {
        let chain = ModelChain::new(Some(Arc::new(MockClient("bitnet"))), None, None);
        assert!(chain.is_offline());
        let sel = chain.select(&classification(false));
        assert_eq!(sel.tier_label(), "bitnet");
    }

    #[test]
    fn no_models_returns_none() {
        let chain = ModelChain::new(None, None, None);
        assert!(chain.select(&classification(false)).is_none());
        assert_eq!(chain.status().mode, "no models");
    }

    #[test]
    fn status_connected() {
        let chain = ModelChain::new(None, Some(Arc::new(MockClient("c"))), None);
        assert_eq!(chain.status().mode, "connected");
    }
}
