use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Safety assessment result from control plane for optimization decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OptimizationSafety {
    /// Safe to proceed with the optimization action.
    Ready,
    /// Cannot proceed safely due to insufficient data.
    InsufficientData,
    /// The proposed solution is unsafe and must not be executed.
    UnsafeSolution,
}

/// Evidence request for insufficient data scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRequest {
    /// Unique request identifier.
    pub id: String,
    /// Wall-clock time the request was created.
    pub timestamp: DateTime<Utc>,
    /// Description of the missing evidence/data.
    pub description: String,
    /// List of specific data points or evidence types needed.
    pub required_evidence: Vec<String>,
    /// Context or action that triggered the evidence request.
    pub context: String,
}

/// Telemetry record for blocked optimization execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationTelemetry {
    /// Unique telemetry record identifier.
    pub id: String,
    /// Wall-clock time the blocking occurred.
    pub timestamp: DateTime<Utc>,
    /// The action that was blocked.
    pub blocked_action: String,
    /// The safety assessment that caused the block.
    pub safety_status: OptimizationSafety,
    /// Optional evidence request generated for insufficient data.
    pub evidence_request_id: Option<String>,
    /// Final outcome (if available) - whether action was eventually executed.
    pub eventual_outcome: Option<String>,
}

impl EvidenceRequest {
    /// Create a new evidence request.
    pub fn new(
        description: impl Into<String>,
        required_evidence: Vec<String>,
        context: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            description: description.into(),
            required_evidence,
            context: context.into(),
        }
    }
}

impl OptimizationTelemetry {
    /// Create a new telemetry record for a blocked optimization.
    pub fn new(
        blocked_action: impl Into<String>,
        safety_status: OptimizationSafety,
        evidence_request_id: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            blocked_action: blocked_action.into(),
            safety_status,
            evidence_request_id,
            eventual_outcome: None,
        }
    }

    /// Update the eventual outcome of this blocked action.
    pub fn set_eventual_outcome(&mut self, outcome: impl Into<String>) {
        self.eventual_outcome = Some(outcome.into());
    }
}

/// Optimization safety gate for runtime enforcement.
///
/// Provides safety checking for optimization actions with evidence request
/// generation and telemetry collection for blocked executions.
#[derive(Clone)]
pub struct OptimizationSafetyGate {
    evidence_requests: Arc<Mutex<Vec<EvidenceRequest>>>,
    telemetry: Arc<Mutex<Vec<OptimizationTelemetry>>>,
    /// Maximum number of telemetry records to keep in memory.
    max_telemetry_records: usize,
}

impl Default for OptimizationSafetyGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizationSafetyGate {
    /// Create a new optimization safety gate.
    pub fn new() -> Self {
        Self {
            evidence_requests: Arc::new(Mutex::new(Vec::new())),
            telemetry: Arc::new(Mutex::new(Vec::new())),
            max_telemetry_records: 1000,
        }
    }

    /// Check safety status for an optimization action.
    ///
    /// In a real implementation, this would call out to a control plane.
    /// For now, we provide a placeholder that demonstrates the interface.
    pub fn check_optimization_safety(&self, action: &str) -> OptimizationSafety {
        // Placeholder logic - real implementation would query control plane
        if action.contains("delete") || action.contains("remove") {
            OptimizationSafety::UnsafeSolution
        } else if action.contains("experimental") || action.contains("beta") {
            OptimizationSafety::InsufficientData
        } else {
            OptimizationSafety::Ready
        }
    }

    /// Execute an optimization action with safety enforcement.
    ///
    /// Returns Ok(result) if action is safe to proceed, or Err with blocking reason.
    pub async fn execute_with_safety_check<F, T, E>(
        &self,
        action: &str,
        executor: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        let safety = self.check_optimization_safety(action);

        match safety {
            OptimizationSafety::Ready => {
                // Safe to proceed
                tracing::info!(action, "optimization safety: ready to proceed");
                executor().map_err(|e| format!("execution failed: {}", e))
            }
            OptimizationSafety::InsufficientData => {
                // Generate evidence request and block
                let evidence_req = self.request_evidence(
                    format!("Insufficient data for action: {}", action),
                    vec!["safety_metrics".into(), "impact_analysis".into()],
                    action.to_string(),
                );
                let telemetry = OptimizationTelemetry::new(
                    action,
                    safety.clone(),
                    Some(evidence_req.id.clone()),
                );
                self.record_telemetry(telemetry);

                Err(format!(
                    "Action blocked due to insufficient data. Evidence request: {}",
                    evidence_req.id
                ))
            }
            OptimizationSafety::UnsafeSolution => {
                // Block unsafe action
                let telemetry = OptimizationTelemetry::new(action, safety.clone(), None);
                self.record_telemetry(telemetry);

                Err(format!(
                    "Action blocked: unsafe solution detected for '{}'",
                    action
                ))
            }
        }
    }

    /// Generate an evidence request for insufficient data scenarios.
    pub fn request_evidence(
        &self,
        description: impl Into<String>,
        required_evidence: Vec<String>,
        context: impl Into<String>,
    ) -> EvidenceRequest {
        let request = EvidenceRequest::new(description, required_evidence, context);
        self.evidence_requests.lock().unwrap().push(request.clone());

        tracing::warn!(
            request_id = %request.id,
            description = %request.description,
            "optimization safety: evidence request generated"
        );
        request
    }

    /// Record telemetry for a blocked optimization action.
    pub fn record_telemetry(&self, telemetry: OptimizationTelemetry) {
        let mut records = self.telemetry.lock().unwrap();
        records.push(telemetry.clone());

        // Keep only the most recent records
        if records.len() > self.max_telemetry_records {
            let len = records.len();
            records.drain(0..len - self.max_telemetry_records);
        }

        tracing::warn!(
            telemetry_id = %telemetry.id,
            action = %telemetry.blocked_action,
            safety_status = ?telemetry.safety_status,
            "optimization safety: action blocked"
        );
    }

    /// Get all pending evidence requests.
    pub fn get_pending_evidence_requests(&self) -> Vec<EvidenceRequest> {
        self.evidence_requests.lock().unwrap().clone()
    }

    /// Get recent telemetry records.
    pub fn get_telemetry(&self, limit: Option<usize>) -> Vec<OptimizationTelemetry> {
        let records = self.telemetry.lock().unwrap();
        let limit = limit.unwrap_or(50);
        if records.len() <= limit {
            records.clone()
        } else {
            records[records.len() - limit..].to_vec()
        }
    }

    /// Update the eventual outcome for a telemetry record.
    pub fn update_telemetry_outcome(
        &self,
        telemetry_id: &str,
        outcome: impl Into<String>,
    ) -> Result<(), String> {
        let mut records = self.telemetry.lock().unwrap();
        if let Some(record) = records.iter_mut().find(|r| r.id == telemetry_id) {
            record.set_eventual_outcome(outcome);
            tracing::info!(
                telemetry_id,
                outcome = ?record.eventual_outcome,
                "optimization safety: telemetry outcome updated"
            );
            Ok(())
        } else {
            Err(format!("Telemetry record not found: {}", telemetry_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_gate_creation() {
        let gate = OptimizationSafetyGate::new();
        assert!(gate.get_pending_evidence_requests().is_empty());
        assert!(gate.get_telemetry(None).is_empty());
    }

    #[test]
    fn safety_check_ready_for_safe_actions() {
        let gate = OptimizationSafetyGate::new();
        let safety = gate.check_optimization_safety("optimize_database");
        assert_eq!(safety, OptimizationSafety::Ready);
    }

    #[test]
    fn safety_check_unsafe_for_dangerous_actions() {
        let gate = OptimizationSafetyGate::new();
        let safety = gate.check_optimization_safety("delete_all_data");
        assert_eq!(safety, OptimizationSafety::UnsafeSolution);
    }

    #[test]
    fn safety_check_insufficient_data_for_experimental() {
        let gate = OptimizationSafetyGate::new();
        let safety = gate.check_optimization_safety("experimental_feature");
        assert_eq!(safety, OptimizationSafety::InsufficientData);
    }

    #[tokio::test]
    async fn execute_with_safety_allows_ready_actions() {
        let gate = OptimizationSafetyGate::new();
        let result = gate
            .execute_with_safety_check("safe_optimization", || {
                Ok::<String, String>("success".into())
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
    }

    #[tokio::test]
    async fn execute_with_safety_blocks_unsafe_actions() {
        let gate = OptimizationSafetyGate::new();
        let result = gate
            .execute_with_safety_check("delete_everything", || {
                Ok::<String, String>("should_not_execute".into())
            })
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsafe solution"));
    }

    #[tokio::test]
    async fn execute_with_safety_blocks_insufficient_data() {
        let gate = OptimizationSafetyGate::new();
        let result = gate
            .execute_with_safety_check("experimental_ai", || {
                Ok::<String, String>("should_not_execute".into())
            })
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("insufficient data"));

        // Should generate evidence request
        let requests = gate.get_pending_evidence_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].description.contains("experimental_ai"));
    }

    #[test]
    fn evidence_request_creation() {
        let gate = OptimizationSafetyGate::new();
        let req = gate.request_evidence(
            "Need more data",
            vec!["metric1".into(), "metric2".into()],
            "test_action",
        );

        assert!(!req.id.is_empty());
        assert_eq!(req.description, "Need more data");
        assert_eq!(req.required_evidence, vec!["metric1", "metric2"]);
        assert_eq!(req.context, "test_action");

        let pending = gate.get_pending_evidence_requests();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, req.id);
    }

    #[test]
    fn telemetry_recording() {
        let gate = OptimizationSafetyGate::new();
        let telemetry =
            OptimizationTelemetry::new("blocked_action", OptimizationSafety::UnsafeSolution, None);
        let telemetry_id = telemetry.id.clone();

        gate.record_telemetry(telemetry);

        let records = gate.get_telemetry(None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].blocked_action, "blocked_action");

        // Test outcome update
        let result = gate.update_telemetry_outcome(&telemetry_id, "blocked_permanently");
        assert!(result.is_ok());

        let updated_records = gate.get_telemetry(None);
        assert_eq!(
            updated_records[0].eventual_outcome,
            Some("blocked_permanently".into())
        );
    }

    #[test]
    fn telemetry_limit_enforcement() {
        let mut gate = OptimizationSafetyGate::new();
        gate.max_telemetry_records = 3;

        // Add 5 records
        for i in 0..5 {
            let telemetry = OptimizationTelemetry::new(
                format!("action_{}", i),
                OptimizationSafety::Ready,
                None,
            );
            gate.record_telemetry(telemetry);
        }

        let records = gate.get_telemetry(None);
        assert_eq!(records.len(), 3); // Only kept last 3
        assert_eq!(records[0].blocked_action, "action_2");
        assert_eq!(records[2].blocked_action, "action_4");
    }
}
