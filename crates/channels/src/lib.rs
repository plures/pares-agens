#![warn(missing_docs)]
//! Channel adapters for Pares Agens.
//!
//! Provides the [`ChannelAdapter`] trait and concrete adapter implementations
//! for stdin, Tauri IPC, and Telegram. A [`ChannelAdapter`] bridges an external
//! communication channel (e.g. a Telegram bot, a desktop UI, or standard input)
//! to the core agent event loop.
//!
//! # License gating
//!
//! Running more than one adapter simultaneously requires a Pro license.
//! Use [`check_channel_count`] to enforce this at startup.

pub mod adapter;
pub mod group_context;
pub mod stdin;
pub mod tauri_ipc;
pub mod telegram;

/// Validate that the license permits running the given number of channel adapters.
///
/// Allows zero or one adapter on any tier.  More than one adapter requires a
/// valid Pro license (`Feature::MultipleChannels`).
///
/// # Errors
///
/// Returns [`pares_agens_core::license::LicenseError::FeatureNotAvailable`]
/// when `adapter_count > 1` and the license is not Pro.
///
/// # Example
///
/// ```rust
/// use pares_agens_channels::check_channel_count;
/// use pares_agens_core::license::License;
///
/// // Single adapter — always permitted.
/// check_channel_count(1, &License::free()).expect("single adapter is free");
///
/// // Two adapters — requires Pro.
/// let err = check_channel_count(2, &License::free());
/// assert!(err.is_err(), "free tier cannot run two adapters");
/// ```
pub fn check_channel_count(
    adapter_count: usize,
    license: &pares_agens_core::license::License,
) -> std::result::Result<(), pares_agens_core::license::LicenseError> {
    if adapter_count > 1 {
        license.check_feature(pares_agens_core::license::Feature::MultipleChannels)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_core::license::License;

    #[test]
    fn zero_adapters_allowed_on_free_tier() {
        assert!(check_channel_count(0, &License::free()).is_ok());
    }

    #[test]
    fn single_adapter_allowed_on_free_tier() {
        assert!(check_channel_count(1, &License::free()).is_ok());
    }

    #[test]
    fn multiple_adapters_blocked_on_free_tier() {
        let result = check_channel_count(2, &License::free());
        assert!(
            matches!(
                result,
                Err(pares_agens_core::license::LicenseError::FeatureNotAvailable { .. })
            ),
            "free tier should not allow multiple adapters"
        );
    }

    #[test]
    fn multiple_adapters_allowed_on_pro_tier() {
        assert!(
            check_channel_count(5, &License::pro(None)).is_ok(),
            "pro tier should allow multiple adapters"
        );
    }
}
