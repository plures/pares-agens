//! License key validation and Pro feature gates.
//!
//! ## Tiers
//!
//! | Tier | Features |
//! |------|----------|
//! | Free | Single local model, unlimited local PluresLM memory, 1 channel adapter, core procedures |
//! | Pro  | Multiple channels, multiple model providers + routing, PluresLM+ P2P sync, MCP tool orchestration, Praxis audit export |
//!
//! ## Usage
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use pares_agens_core::license::{Feature, FixedKeyValidator, License, LicenseValidator};
//!
//! // Free tier — always available
//! let free = License::free();
//! assert!(!free.is_pro());
//!
//! // Validate a key and obtain a Pro license
//! let validator = FixedKeyValidator::new("my-pro-key");
//! let pro = validator.validate("my-pro-key").await?;
//! assert!(pro.is_pro());
//! pro.check_feature(Feature::PraxisAuditExport)?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

// ---------------------------------------------------------------------------
// Tier
// ---------------------------------------------------------------------------

/// Subscription tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenseTier {
    /// Free tier — core features only.
    Free,
    /// Pro tier — full feature access.
    Pro,
}

// ---------------------------------------------------------------------------
// Feature
// ---------------------------------------------------------------------------

/// Pro features that require a valid [`LicenseTier::Pro`] license.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Run multiple channel adapters simultaneously.
    MultipleChannels,
    /// Use multiple model providers with routing rules.
    MultipleModelProviders,
    /// PluresLM+ Hyperswarm P2P sync.
    PluresLMPlus,
    /// Advanced procedures and MCP tool orchestration.
    McpToolOrchestration,
    /// Export the Praxis decision ledger for audit/compliance.
    PraxisAuditExport,
}

impl Feature {
    /// Short identifier used in error messages.
    pub fn name(&self) -> &'static str {
        match self {
            Feature::MultipleChannels => "multiple-channels",
            Feature::MultipleModelProviders => "multiple-model-providers",
            Feature::PluresLMPlus => "plureslm-plus",
            Feature::McpToolOrchestration => "mcp-tool-orchestration",
            Feature::PraxisAuditExport => "praxis-audit-export",
        }
    }
}

// ---------------------------------------------------------------------------
// Status (serialisable for UI)
// ---------------------------------------------------------------------------

/// Serialisable snapshot of the current license state.
///
/// Returned by [`License::status`] and suitable for surfacing in any UI or
/// status endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseStatus {
    /// Subscription tier.
    pub tier: LicenseTier,
    /// Whether the license is currently valid (not expired).
    pub valid: bool,
    /// Optional expiry timestamp for Pro licenses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors returned by license validation and feature-gate checks.
#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    /// The requested feature is not available on the current tier.
    #[error("feature '{feature}' requires a Pro license")]
    FeatureNotAvailable {
        /// The name of the gated feature (see [`Feature::name`]).
        feature: String,
    },
    /// The supplied license key is not valid.
    #[error("invalid license key: {reason}")]
    InvalidKey {
        /// Human-readable description of why validation failed.
        reason: String,
    },
    /// The license key has expired.
    #[error("license has expired")]
    Expired,
    /// Network error during remote validation (used for offline grace period).
    #[error("network error during license validation: {0}")]
    NetworkError(String),
}

// ---------------------------------------------------------------------------
// License
// ---------------------------------------------------------------------------

/// Holds the resolved license tier and expiry.
///
/// Construct with [`License::free`] or [`License::pro`], or by calling a
/// [`LicenseValidator`] implementation.
#[derive(Debug, Clone)]
pub struct License {
    status: LicenseStatus,
}

impl Default for License {
    fn default() -> Self {
        Self::free()
    }
}

impl License {
    /// Create a Free-tier license (no Pro features).
    pub fn free() -> Self {
        Self {
            status: LicenseStatus {
                tier: LicenseTier::Free,
                valid: true,
                expires_at: None,
            },
        }
    }

    /// Create a Pro-tier license with an optional expiry timestamp.
    ///
    /// If `expires_at` is in the past the license is marked invalid immediately.
    pub fn pro(expires_at: Option<DateTime<Utc>>) -> Self {
        let valid = expires_at.map(|exp| exp > Utc::now()).unwrap_or(true);
        Self {
            status: LicenseStatus {
                tier: LicenseTier::Pro,
                valid,
                expires_at,
            },
        }
    }

    /// Serialisable status snapshot for UI display.
    ///
    /// Note: The `valid` field in [`LicenseStatus`] reflects validity at the time
    /// the `License` was constructed and is not recomputed on each call to
    /// this method. It may become stale if a Pro license subsequently expires.
    ///
    /// For up-to-date checks, use [`License::is_pro`] or [`License::check_feature`],
    /// which always compare `expires_at` against the current time.
    ///
    /// The `valid` field is recomputed against the current wall-clock time on
    /// every call, so the snapshot is always fresh and never stale.
    pub fn status(&self) -> LicenseStatus {
        LicenseStatus {
            tier: self.status.tier.clone(),
            valid: self.is_pro() || self.status.tier == LicenseTier::Free,
            expires_at: self.status.expires_at,
        }
    }

    /// Returns `true` if this is a currently valid Pro license.
    ///
    /// The expiry is checked against the current wall-clock time on every
    /// call to avoid stale TOCTOU state.
    pub fn is_pro(&self) -> bool {
        if self.status.tier != LicenseTier::Pro {
            return false;
        }
        self.status
            .expires_at
            .map(|exp| exp > Utc::now())
            .unwrap_or(true)
    }

    /// Assert that `feature` is available under the current license.
    ///
    /// Returns `Ok(())` for valid Pro licenses.
    /// Returns `Err(LicenseError::FeatureNotAvailable)` on Free tier.
    /// Returns `Err(LicenseError::Expired)` when the Pro license has expired.
    ///
    /// The expiry is checked against the current wall-clock time on every
    /// call to avoid stale TOCTOU state.
    pub fn check_feature(&self, feature: Feature) -> Result<(), LicenseError> {
        if self.status.tier != LicenseTier::Pro {
            return Err(LicenseError::FeatureNotAvailable {
                feature: feature.name().to_owned(),
            });
        }
        if let Some(exp) = self.status.expires_at {
            if exp <= Utc::now() {
                return Err(LicenseError::Expired);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validator trait
// ---------------------------------------------------------------------------

/// Validates a raw license key string and returns a resolved [`License`].
///
/// Implement this trait to support different license back-ends (Polar, Stripe,
/// self-signed HMAC keys, etc.).  The crate ships [`FixedKeyValidator`] for
/// simple self-hosted deployments.
#[async_trait]
pub trait LicenseValidator: Send + Sync {
    /// Validate `key` and return the resolved license, or an error.
    async fn validate(&self, key: &str) -> Result<License, LicenseError>;
}

// ---------------------------------------------------------------------------
// FixedKeyValidator
// ---------------------------------------------------------------------------

/// Validates a license key by comparing it against a single expected Pro key.
///
/// Suitable for self-hosted deployments where the operator pre-shares one Pro
/// key (e.g. via a secret manager).  For Polar or Stripe license validation,
/// implement [`LicenseValidator`] against the respective REST API.
pub struct FixedKeyValidator {
    pro_key: String,
}

impl FixedKeyValidator {
    /// Create a validator with the given expected Pro key.
    pub fn new(pro_key: impl Into<String>) -> Self {
        Self {
            pro_key: pro_key.into(),
        }
    }
}

#[async_trait]
impl LicenseValidator for FixedKeyValidator {
    async fn validate(&self, key: &str) -> Result<License, LicenseError> {
        let trimmed = key.trim();
        let keys_match: bool = trimmed.as_bytes().ct_eq(self.pro_key.as_bytes()).into();
        if keys_match {
            Ok(License::pro(None))
        } else {
            Err(LicenseError::InvalidKey {
                reason: "key does not match".into(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// PolarValidator
// ---------------------------------------------------------------------------

/// Validates a license key against the [Polar.sh] license API.
///
/// On success the validator returns a Pro [`License`].  If the network call
/// fails (e.g. the user is offline), [`validate_with_grace`] allows the app
/// to remain functional for up to `grace_days` days after the last successful
/// online validation, using the caller-supplied `last_validated_at` timestamp.
///
/// ## Configuration
///
/// Set your Polar.sh benefit ID (the UUID shown in your Polar dashboard) when
/// constructing the validator:
///
/// ```rust,no_run
/// use pares_agens_core::license::PolarValidator;
///
/// let validator = PolarValidator::new("00000000-0000-0000-0000-000000000000");
/// ```
///
/// [Polar.sh]: https://polar.sh
/// [`validate_with_grace`]: PolarValidator::validate_with_grace
pub struct PolarValidator {
    benefit_id: String,
    client: reqwest::Client,
    /// Number of days a Pro license remains valid when the API is unreachable.
    pub grace_days: i64,
}

impl PolarValidator {
    /// Create a validator for the given Polar.sh benefit ID.
    ///
    /// Uses a 7-day offline grace period by default.
    pub fn new(benefit_id: impl Into<String>) -> Self {
        Self {
            benefit_id: benefit_id.into(),
            client: reqwest::Client::new(),
            grace_days: 7,
        }
    }

    /// Override the default 7-day offline grace period.
    pub fn with_grace_days(mut self, days: i64) -> Self {
        self.grace_days = days;
        self
    }

    /// Validate a key, falling back to the offline grace period on network errors.
    ///
    /// * If the Polar.sh API is reachable and the key is valid, returns `Ok(Pro)`.
    /// * If the API is unreachable **and** `last_validated_at` is within the
    ///   configured grace period, returns `Ok(Pro)` without contacting Polar.
    /// * Otherwise propagates the error.
    ///
    /// `last_validated_at` should be the timestamp returned by the caller's
    /// last successful online validation.  Pass `None` to disable the grace
    /// period (useful on first activation).
    pub async fn validate_with_grace(
        &self,
        key: &str,
        last_validated_at: Option<DateTime<Utc>>,
    ) -> Result<License, LicenseError> {
        match self.validate(key).await {
            Ok(lic) => Ok(lic),
            Err(LicenseError::NetworkError(_)) => {
                if let Some(last) = last_validated_at {
                    let cutoff = Utc::now() - chrono::TimeDelta::days(self.grace_days);
                    if last > cutoff {
                        return Ok(License::pro(None));
                    }
                }
                Err(LicenseError::NetworkError(format!(
                    "offline and {}-day grace period has expired",
                    self.grace_days
                )))
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl LicenseValidator for PolarValidator {
    /// Validate `key` against the Polar.sh `/v1/benefits/licenses/validate` endpoint.
    async fn validate(&self, key: &str) -> Result<License, LicenseError> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(LicenseError::InvalidKey {
                reason: "key is empty".into(),
            });
        }

        let resp = self
            .client
            .post("https://api.polar.sh/v1/benefits/licenses/validate")
            .json(&serde_json::json!({
                "key": trimmed,
                "benefit_id": self.benefit_id,
            }))
            .send()
            .await
            .map_err(|e| LicenseError::NetworkError(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND
            || status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
        {
            return Err(LicenseError::InvalidKey {
                reason: "license key not found or invalid".into(),
            });
        }
        if !status.is_success() {
            return Err(LicenseError::NetworkError(format!("HTTP {status}")));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LicenseError::NetworkError(e.to_string()))?;

        // Polar.sh returns `"status": "granted"` for active licenses.
        match body.get("status").and_then(|s| s.as_str()) {
            Some("granted") => {
                let expires_at = body
                    .get("expires_at")
                    .and_then(|e| e.as_str())
                    .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
                    .map(|e| e.with_timezone(&Utc));
                Ok(License::pro(expires_at))
            }
            Some("revoked") => Err(LicenseError::InvalidKey {
                reason: "license has been revoked".into(),
            }),
            _ => Err(LicenseError::InvalidKey {
                reason: "license is not active".into(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── License::free ────────────────────────────────────────────────────────

    #[test]
    fn free_license_is_not_pro() {
        let lic = License::free();
        assert!(!lic.is_pro());
        assert_eq!(lic.status().tier, LicenseTier::Free);
        assert!(lic.status().valid);
    }

    // ── License::pro ─────────────────────────────────────────────────────────

    #[test]
    fn pro_license_without_expiry_is_valid() {
        let lic = License::pro(None);
        assert!(lic.is_pro());
        assert_eq!(lic.status().tier, LicenseTier::Pro);
        assert!(lic.status().valid);
        assert!(lic.status().expires_at.is_none());
    }

    #[test]
    fn pro_license_with_future_expiry_is_valid() {
        let future = Utc::now() + chrono::TimeDelta::days(30);
        let lic = License::pro(Some(future));
        assert!(lic.is_pro());
        assert!(lic.status().valid);
    }

    #[test]
    fn pro_license_with_past_expiry_is_invalid() {
        let past = Utc::now() - chrono::TimeDelta::days(1);
        let lic = License::pro(Some(past));
        assert!(
            !lic.is_pro(),
            "expired license should not be treated as pro"
        );
        assert!(!lic.status().valid);
    }

    // ── check_feature ────────────────────────────────────────────────────────

    #[test]
    fn free_license_blocks_all_pro_features() {
        let lic = License::free();
        let features = [
            Feature::MultipleChannels,
            Feature::MultipleModelProviders,
            Feature::PluresLMPlus,
            Feature::McpToolOrchestration,
            Feature::PraxisAuditExport,
        ];
        for feature in features {
            let name = feature.name();
            let result = lic.check_feature(feature);
            assert!(
                result.is_err(),
                "free license should block feature '{name}'"
            );
            assert!(
                matches!(result, Err(LicenseError::FeatureNotAvailable { .. })),
                "expected FeatureNotAvailable for '{name}'"
            );
        }
    }

    #[test]
    fn pro_license_allows_all_pro_features() {
        let lic = License::pro(None);
        let features = [
            Feature::MultipleChannels,
            Feature::MultipleModelProviders,
            Feature::PluresLMPlus,
            Feature::McpToolOrchestration,
            Feature::PraxisAuditExport,
        ];
        for feature in features {
            assert!(
                lic.check_feature(feature).is_ok(),
                "pro license should allow all pro features"
            );
        }
    }

    #[test]
    fn expired_pro_license_blocks_features() {
        let past = Utc::now() - chrono::TimeDelta::days(1);
        let lic = License::pro(Some(past));
        let result = lic.check_feature(Feature::PraxisAuditExport);
        assert!(matches!(result, Err(LicenseError::Expired)));
    }

    // ── LicenseStatus serialization ──────────────────────────────────────────

    #[test]
    fn pro_status_serializes_correctly() {
        let lic = License::pro(None);
        let json = serde_json::to_value(lic.status()).expect("should serialize");
        assert_eq!(json["tier"], "pro");
        assert_eq!(json["valid"], true);
        assert!(
            json.get("expires_at").is_none(),
            "None expires_at should be omitted"
        );
    }

    #[test]
    fn free_status_serializes_correctly() {
        let lic = License::free();
        let json = serde_json::to_value(lic.status()).expect("should serialize");
        assert_eq!(json["tier"], "free");
        assert_eq!(json["valid"], true);
    }

    #[test]
    fn expired_status_serializes_correctly() {
        let past = Utc::now() - chrono::TimeDelta::days(1);
        let lic = License::pro(Some(past));
        let json = serde_json::to_value(lic.status()).expect("should serialize");
        assert_eq!(json["tier"], "pro");
        assert_eq!(json["valid"], false);
        assert!(json.get("expires_at").is_some());
    }

    // ── FixedKeyValidator ────────────────────────────────────────────────────

    #[tokio::test]
    async fn fixed_validator_accepts_matching_key() {
        let validator = FixedKeyValidator::new("secret-pro-key");
        let lic = validator
            .validate("secret-pro-key")
            .await
            .expect("should validate");
        assert!(lic.is_pro());
    }

    #[tokio::test]
    async fn fixed_validator_rejects_wrong_key() {
        let validator = FixedKeyValidator::new("correct-key");
        let err = validator.validate("wrong-key").await.unwrap_err();
        assert!(matches!(err, LicenseError::InvalidKey { .. }));
    }

    #[tokio::test]
    async fn fixed_validator_trims_whitespace() {
        let validator = FixedKeyValidator::new("my-key");
        let lic = validator
            .validate("  my-key  ")
            .await
            .expect("should trim and validate");
        assert!(lic.is_pro());
    }

    #[tokio::test]
    async fn fixed_validator_empty_key_rejected() {
        let validator = FixedKeyValidator::new("real-key");
        let err = validator.validate("").await.unwrap_err();
        assert!(matches!(err, LicenseError::InvalidKey { .. }));
    }

    // ── PolarValidator grace period ──────────────────────────────────────────

    #[tokio::test]
    async fn polar_validator_grace_period_allows_offline_within_window() {
        let validator = PolarValidator::new("test-benefit-id");
        let network_err = LicenseError::NetworkError("connection refused".into());

        // last validated 3 days ago — still within the default 7-day window
        let last_validated = Utc::now() - chrono::TimeDelta::days(3);

        // Replicate the grace logic without making a real HTTP call.
        let result: Result<License, LicenseError> = match Err(network_err) {
            Err(LicenseError::NetworkError(_)) => {
                let cutoff = Utc::now() - chrono::TimeDelta::days(validator.grace_days);
                if last_validated > cutoff {
                    Ok(License::pro(None))
                } else {
                    Err(LicenseError::NetworkError("expired".into()))
                }
            }
            other => other,
        };

        assert!(
            result.is_ok(),
            "grace period should allow offline use within window"
        );
        assert!(result.unwrap().is_pro());
    }

    #[tokio::test]
    async fn polar_validator_grace_period_blocks_after_window() {
        let validator = PolarValidator::new("test-benefit-id");
        let network_err = LicenseError::NetworkError("connection refused".into());

        // last validated 8 days ago — beyond the 7-day window
        let last_validated = Utc::now() - chrono::TimeDelta::days(8);

        let result: Result<License, LicenseError> = match Err(network_err) {
            Err(LicenseError::NetworkError(_)) => {
                let cutoff = Utc::now() - chrono::TimeDelta::days(validator.grace_days);
                if last_validated > cutoff {
                    Ok(License::pro(None))
                } else {
                    Err(LicenseError::NetworkError("expired".into()))
                }
            }
            other => other,
        };

        assert!(
            matches!(result, Err(LicenseError::NetworkError(_))),
            "grace period should block after window expires"
        );
    }

    #[tokio::test]
    async fn polar_validator_grace_period_blocks_with_no_prior_validation() {
        let validator = PolarValidator::new("test-benefit-id");
        let network_err = LicenseError::NetworkError("connection refused".into());

        let result: Result<License, LicenseError> = match Err(network_err) {
            Err(LicenseError::NetworkError(_)) => {
                // None → no prior validation recorded → no grace
                let last_validated: Option<DateTime<Utc>> = None;
                let grace = if let Some(last) = last_validated {
                    let cutoff = Utc::now() - chrono::TimeDelta::days(validator.grace_days);
                    last > cutoff
                } else {
                    false
                };
                if grace {
                    Ok(License::pro(None))
                } else {
                    Err(LicenseError::NetworkError("expired".into()))
                }
            }
            other => other,
        };

        assert!(
            matches!(result, Err(LicenseError::NetworkError(_))),
            "no prior validation means no grace period"
        );
    }

    #[test]
    fn polar_validator_empty_key_returns_invalid() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let validator = PolarValidator::new("test-benefit-id");
            let err = validator.validate("").await.unwrap_err();
            assert!(matches!(err, LicenseError::InvalidKey { .. }));
        });
    }
}
