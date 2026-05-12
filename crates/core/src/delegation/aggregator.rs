//! Result aggregation — merges outputs from multiple sub-agents.

use crate::delegation::broker::SubTaskResult;

/// The merged output produced by [`ResultAggregator::aggregate`].
#[derive(Debug, Clone)]
pub struct AggregatedResult {
    /// The combined textual output of all successful sub-agents.
    pub content: String,
    /// Names of agents that completed successfully.
    pub succeeded: Vec<String>,
    /// Names of agents (and their error messages) that failed.
    pub failed: Vec<(String, String)>,
}

impl AggregatedResult {
    /// `true` if every sub-agent completed successfully.
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }

    /// `true` if at least one sub-agent produced output.
    pub fn has_output(&self) -> bool {
        !self.content.is_empty()
    }
}

// ── ResultAggregator ─────────────────────────────────────────────────────────

/// Merges a collection of [`SubTaskResult`]s into a single [`AggregatedResult`].
///
/// The default strategy concatenates each agent's output under a Markdown
/// heading named after the agent.  Callers that need a different merging
/// strategy (e.g. majority-vote, LLM synthesis pass) should process the
/// individual [`SubTaskResult`]s themselves.
#[derive(Default)]
pub struct ResultAggregator;

impl ResultAggregator {
    /// Create a new aggregator.
    pub fn new() -> Self {
        Self
    }

    /// Merge `results` into an [`AggregatedResult`].
    ///
    /// When only one agent succeeds, its output is returned directly without
    /// any wrapper heading — the user sees a clean, coherent response.
    ///
    /// When multiple agents succeed, outputs are concatenated under per-agent
    /// headings. A future improvement should run a synthesis pass through a
    /// cheap model to merge them into a single voice.
    pub fn aggregate(&self, results: Vec<SubTaskResult>) -> AggregatedResult {
        let mut sections: Vec<(String, String)> = Vec::new();
        let mut succeeded: Vec<String> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();

        for result in results {
            match result.output {
                Ok(output) if !output.trim().is_empty() => {
                    sections.push((result.agent_name.clone(), output.trim().to_string()));
                    succeeded.push(result.agent_name);
                }
                Ok(_) => {
                    // Agent succeeded but produced no output — still mark as
                    // succeeded to distinguish from an error.
                    succeeded.push(result.agent_name);
                }
                Err(err) => {
                    failed.push((result.agent_name, err));
                }
            }
        }

        // Single agent: return its output directly, no header wrapper.
        // This produces a clean response that reads like one voice.
        let content = if sections.len() == 1 {
            sections.into_iter().next().unwrap().1
        } else {
            sections
                .into_iter()
                .map(|(name, output)| format!("## {}\n\n{}", name, output))
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        AggregatedResult {
            content,
            succeeded,
            failed,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::broker::SubTaskResult;

    fn ok(agent: &str, output: &str) -> SubTaskResult {
        SubTaskResult {
            agent_name: agent.to_string(),
            output: Ok(output.to_string()),
        }
    }

    fn err(agent: &str, msg: &str) -> SubTaskResult {
        SubTaskResult {
            agent_name: agent.to_string(),
            output: Err(msg.to_string()),
        }
    }

    #[test]
    fn aggregate_all_success() {
        let agg = ResultAggregator::new();
        let result = agg.aggregate(vec![
            ok("researcher", "Found 3 papers."),
            ok("analyst", "Analysis complete."),
        ]);
        assert!(result.all_succeeded());
        assert!(result.has_output());
        assert!(result.content.contains("## researcher"));
        assert!(result.content.contains("## analyst"));
        assert!(result.content.contains("Found 3 papers."));
    }

    #[test]
    fn single_agent_no_header() {
        let agg = ResultAggregator::new();
        let result = agg.aggregate(vec![
            ok("coder", "Here is the implementation."),
        ]);
        assert!(result.all_succeeded());
        assert!(result.has_output());
        assert!(!result.content.contains("## coder"), "single agent output should not have a header");
        assert_eq!(result.content, "Here is the implementation.");
    }

    #[test]
    fn aggregate_mixed_success_and_failure() {
        let agg = ResultAggregator::new();
        let result = agg.aggregate(vec![
            ok("researcher", "some output"),
            err("coder", "model timeout"),
        ]);
        assert!(!result.all_succeeded());
        assert_eq!(result.succeeded, vec!["researcher"]);
        assert_eq!(
            result.failed,
            vec![("coder".to_string(), "model timeout".to_string())]
        );
    }

    #[test]
    fn aggregate_empty_output_is_success_without_content() {
        let agg = ResultAggregator::new();
        let result = agg.aggregate(vec![ok("analyst", "   ")]);
        assert!(result.all_succeeded());
        assert!(!result.has_output());
    }

    #[test]
    fn aggregate_empty_input() {
        let agg = ResultAggregator::new();
        let result = agg.aggregate(vec![]);
        assert!(result.all_succeeded());
        assert!(!result.has_output());
        assert!(result.succeeded.is_empty());
    }
}
