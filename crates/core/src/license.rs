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
    FeatureNotAvailable { feature: String },
    /// The supplied license key is not valid.
    #[error("invalid license key: {reason}")]
    InvalidKey { reason: String },
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
        self.status.expires_at.map(|exp| exp > Utc::now()).unwrap_or(true)
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
            return Err(LicenseError::FeatureNotAvailable { feature: feature.name().to_owned() });
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
        Self { pro_key: pro_key.into() }
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
            Err(LicenseError::InvalidKey { reason: "key does not match".into() })
        }
    }
}

// ---------------------------------------------------------------------------
// PolarValidator
// ---------------------------------------------------------------------------

/// Validates a license key against the [Polar.sh] license API.
///
/// On success the validator returns a Pro [`License`].  If the network call
/// fails (e.g. the user is offline), [`PolarValidator::validate_with_grace`]
/// allows the app to remain functional for up to `grace_days` days after the
/// last successful online validation, using the caller-supplied
/// `last_validated_at` timestamp.
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
pub struct PolarValidator {
    benefit_id: String,
    client: reqwest::Client,
    /// Number of days the app may run without a successful online validation.
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

    /// Validate `key` against the Polar.sh API.
    ///
    /// Returns `Err(LicenseError::NetworkError)` when the API is unreachable,
    /// allowing the caller to apply the offline grace period via
    /// [`validate_with_grace`].
    ///
    /// [`validate_with_grace`]: PolarValidator::validate_with_grace
    pub async fn validate_online(&self, key: &str) -> Result<License, LicenseError> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(LicenseError::InvalidKey { reason: "key must not be empty".into() });
        }

        let body = serde_json::json!({
            "benefit_id":    self.benefit_id,
            "license_key_id": trimmed,
        });

        let resp = self
            .client
            .post("https://api.polar.sh/v1/benefits/licenses/validate")
            .json(&body)
            .send()
            .await
            .map_err(|e| LicenseError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(LicenseError::InvalidKey {
                reason: format!("Polar API returned HTTP {status}: {text}"),
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LicenseError::NetworkError(e.to_string()))?;

        // Polar returns `"status": "granted"` for valid active licenses.
        if json.get("status").and_then(|s| s.as_str()) == Some("granted") {
            // Parse optional expiry from the response.
            let expires_at = json
                .get("expires_at")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            Ok(License::pro(expires_at))
        } else {
            Err(LicenseError::InvalidKey { reason: "license key not granted".into() })
        }
    }

    /// Validate `key` with an offline grace fallback.
    ///
    /// If the Polar API is unreachable *and* `last_validated_at` is within
    /// `grace_days` of today, the current Pro license is extended silently.
    /// Otherwise the `NetworkError` is propagated so the caller can decide
    /// how to handle it.
    ///
    /// `last_validated_at` should be `None` when the key has never been
    /// validated online successfully — in that case no grace is granted.
    pub async fn validate_with_grace(
        &self,
        key: &str,
        last_validated_at: Option<DateTime<Utc>>,
    ) -> Result<License, LicenseError> {
        match self.validate_online(key).await {
            Ok(lic) => Ok(lic),
            Err(LicenseError::NetworkError(_)) => {
                let grace = if let Some(last) = last_validated_at {
                    let cutoff = Utc::now() - chrono::TimeDelta::days(self.grace_days);
                    last > cutoff
                } else {
                    false
                };
                if grace {
                    Ok(License::pro(None))
                } else {
                    Err(LicenseError::NetworkError(
                        "cannot reach Polar API and grace period has expired or was never set"
                            .into(),
                    ))
                }
            }
            Err(other) => Err(other),
        }
    }
}

#[async_trait]
impl LicenseValidator for PolarValidator {
    async fn validate(&self, key: &str) -> Result<License, LicenseError> {
        self.validate_online(key).await
    }
}

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
        assert!(!lic.is_pro(), "expired license should not be treated as pro");
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
            assert!(result.is_err(), "free license should block feature '{name}'");
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
        assert!(json.get("expires_at").is_none(), "None expires_at should be omitted");
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
        let lic = validator.validate("secret-pro-key").await.expect("should validate");
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
        let lic = validator.validate("  my-key  ").await.expect("should trim and validate");
        assert!(lic.is_pro());
    }

    #[tokio::test]
    async fn fixed_validator_empty_key_rejected() {
        let validator = FixedKeyValidator::new("real-key");
        let err = validator.validate("").await.unwrap_err();
        assert!(matches!(err, LicenseError::InvalidKey { .. }));
    }

    // ── PolarValidator grace period ──────────────────────────────────────────

    /// Simulate the grace-period branch of `validate_with_grace` without making
    /// a real HTTP call, by injecting a pre-built `NetworkError`.
    fn apply_grace(
        grace_days: i64,
        last_validated_at: Option<DateTime<Utc>>,
    ) -> Result<License, LicenseError> {
        let grace = if let Some(last) = last_validated_at {
            let cutoff = Utc::now() - chrono::TimeDelta::days(grace_days);
            last > cutoff
        } else {
            false
        };
        if grace {
            Ok(License::pro(None))
        } else {
            Err(LicenseError::NetworkError(
                "cannot reach Polar API and grace period has expired or was never set".into(),
            ))
        }
    }

    #[tokio::test]
    async fn polar_validator_grace_period_allows_offline_within_window() {
        let validator = PolarValidator::new("test-benefit-id");
        // last validated 3 days ago — still within the 7-day window
        let last = Utc::now() - chrono::TimeDelta::days(3);
        let result = apply_grace(validator.grace_days, Some(last));
        assert!(result.is_ok(), "grace period should allow offline use within window");
        assert!(result.unwrap().is_pro());
    }

    #[tokio::test]
    async fn polar_validator_grace_period_blocks_after_window() {
        let validator = PolarValidator::new("test-benefit-id");
        // last validated 8 days ago — beyond the 7-day window
        let last = Utc::now() - chrono::TimeDelta::days(8);
        let result = apply_grace(validator.grace_days, Some(last));
        assert!(
            matches!(result, Err(LicenseError::NetworkError(_))),
            "grace period should block after window expires"
        );
    }

    #[tokio::test]
    async fn polar_validator_grace_period_blocks_with_no_prior_validation() {
        let validator = PolarValidator::new("test-benefit-id");
        let result = apply_grace(validator.grace_days, None);
        assert!(
            matches!(result, Err(LicenseError::NetworkError(_))),
            "no prior validation means no grace period"
        );
    }
}
