//! Feature-gate helpers — thin, ergonomic wrappers over [`pares_radix_core::license`].
//!
//! Prefer the convenience functions here over calling
//! [`License::check_feature`] directly when you only need a boolean check or a
//! human-readable feature name.
//!
//! ## Example
//!
//! ```rust
//! use pares_agens_core::features;
//! use pares_agens_core::license::{Feature, License};
//!
//! let free = License::free();
//! assert!(!features::is_available(&free, Feature::MultipleChannels));
//!
//! let pro = License::pro(None);
//! assert!(features::is_available(&pro, Feature::MultipleChannels));
//! ```

use pares_radix_core::license::{Feature, License, LicenseError};

/// Assert that `feature` is available under `license`.
///
/// This is a direct delegate to [`License::check_feature`].  It returns
/// `Ok(())` when the feature is allowed and `Err(LicenseError)` otherwise.
#[inline]
pub fn require(license: &License, feature: Feature) -> Result<(), LicenseError> {
    license.check_feature(feature)
}

/// Return `true` if `feature` is available under `license`.
///
/// Equivalent to `license.check_feature(feature).is_ok()`.
#[inline]
pub fn is_available(license: &License, feature: Feature) -> bool {
    license.check_feature(feature).is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_blocks_pro_feature_on_free() {
        let lic = License::free();
        let err = require(&lic, Feature::MultipleChannels).unwrap_err();
        assert!(matches!(err, LicenseError::FeatureNotAvailable { .. }));
    }

    #[test]
    fn require_allows_pro_feature_on_pro() {
        let lic = License::pro(None);
        assert!(require(&lic, Feature::MultipleChannels).is_ok());
    }

    #[test]
    fn is_available_returns_false_for_free_tier() {
        let lic = License::free();
        assert!(!is_available(&lic, Feature::McpToolOrchestration));
    }

    #[test]
    fn is_available_returns_true_for_pro_tier() {
        let lic = License::pro(None);
        assert!(is_available(&lic, Feature::McpToolOrchestration));
    }

    #[test]
    fn is_available_returns_false_for_expired_pro() {
        let past = chrono::Utc::now() - chrono::TimeDelta::days(1);
        let lic = License::pro(Some(past));
        assert!(!is_available(&lic, Feature::PraxisAuditExport));
    }
}
