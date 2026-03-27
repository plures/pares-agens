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
    /// Successful results are concatenated under per-agent Markdown headings.
    /// Failed results are recorded but do not contribute to the content.
    pub fn aggregate(&self, results: Vec<SubTaskResult>) -> AggregatedResult {
        let mut sections: Vec<String> = Vec::new();
        let mut succeeded: Vec<String> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();

        for result in results {
            match result.output {
                Ok(output) if !output.trim().is_empty() => {
                    sections.push(format!("## {}\n\n{}", result.agent_name, output.trim()));
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

        AggregatedResult {
            content: sections.join("\n\n"),
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
