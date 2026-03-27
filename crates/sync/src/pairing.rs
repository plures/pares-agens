//! Pairing flow — generate/share sync keys, manage device approval sessions.
//!
//! The pairing flow works as follows:
//!
//! 1. Device A calls [`PairingSession::new`] to generate a [`SyncKey`] and
//!    a short human-readable [`PairingCode`].
//! 2. The user shares the **full [`SyncKey`]** with Device B out-of-band
//!    (e.g. via QR code or encrypted clipboard).  The [`PairingCode`] is a
//!    display-only label derived from the key; it cannot reconstruct the key.
//! 3. Device B calls [`PairingSession::from_key`] with the received hex key
//!    to construct its view of the session.
//! 4. Device A calls [`PairingSession::approve`] to confirm the pairing.
//! 5. Both devices use the [`SyncKey`] as the Hyperswarm DHT topic seed and
//!    begin announcing on the network.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SyncError;

// ── SyncKey ───────────────────────────────────────────────────────────────────

/// A 32-byte symmetric sync key shared between paired devices.
///
/// The key is represented as a 64-character lowercase hex string.  It is used
/// as the Hyperswarm DHT topic seed so only devices holding the key can find
/// each other on the network.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncKey(String);

impl SyncKey {
    /// Generate a new random [`SyncKey`].
    #[must_use]
    pub fn generate() -> Self {
        // Combine two UUIDs (128 bits each) to produce a 256-bit hex string.
        let a = Uuid::new_v4().as_simple().to_string();
        let b = Uuid::new_v4().as_simple().to_string();
        Self(format!("{a}{b}"))
    }

    /// Parse a [`SyncKey`] from a 64-character hex string.
    ///
    /// The input may use uppercase or lowercase hex digits; it is normalised
    /// to lowercase internally.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::InvalidPairingMaterial`] when the string is not
    /// exactly 64 characters, or when it contains non-hex characters.
    pub fn from_hex(hex: impl Into<String>) -> Result<Self, SyncError> {
        let hex = hex.into();
        if hex.len() != 64 {
            return Err(SyncError::InvalidPairingMaterial(format!(
                "sync key must be exactly 64 hex characters, got {} chars",
                hex.len()
            )));
        }
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SyncError::InvalidPairingMaterial(
                "sync key must contain only hexadecimal characters [0-9a-fA-F]".to_string(),
            ));
        }
        Ok(Self(hex.to_lowercase()))
    }

    /// Return the raw hex string.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Derive a short 6-character pairing code from the first 3 bytes of the key.
    ///
    /// The code is uppercase alphanumeric and is suitable for manual entry or
    /// display in a QR code label.
    #[must_use]
    pub fn to_pairing_code(&self) -> PairingCode {
        // Take the first 6 hex chars → convert to uppercase.
        let short = self.0[..6].to_uppercase();
        PairingCode(short)
    }
}

impl fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mask all but the first 8 chars for safety.
        write!(f, "SyncKey({}...)", &self.0[..8])
    }
}

impl fmt::Display for SyncKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use the same redacted representation as Debug to avoid accidentally
        // leaking the full key into logs or telemetry via `format!("{key}")`.
        // Use `SyncKey::as_hex()` to explicitly retrieve the raw value.
        write!(f, "{self:?}")
    }
}

// ── PairingCode ───────────────────────────────────────────────────────────────

/// A short uppercase alphanumeric code derived from a [`SyncKey`].
///
/// This is the user-facing representation of a pairing invitation, designed
/// for display in a QR code or manual transcription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode(String);

impl PairingCode {
    /// Return the code string (always 6 uppercase characters).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ── ApprovalState ─────────────────────────────────────────────────────────────

/// Lifecycle state of a pairing approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    /// The pairing request has been sent but the local user has not yet acted.
    Pending,
    /// The local user has approved the pairing.
    Approved,
    /// The local user has rejected the pairing.
    Rejected,
}

// ── PairingSession ────────────────────────────────────────────────────────────

/// An in-progress device pairing session.
///
/// Created by the initiating device, passed to the receiving device via the
/// [`PairingCode`], and finalised by the initiating device calling
/// [`PairingSession::approve`] or [`PairingSession::reject`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingSession {
    /// Unique session identifier (UUID v4 string).
    pub id: String,

    /// The full sync key for this session.
    pub sync_key: SyncKey,

    /// The short code the remote device must enter.
    pub pairing_code: PairingCode,

    /// Human-readable name of the device initiating the pairing.
    pub initiator_name: String,

    /// Current approval state.
    pub state: ApprovalState,

    /// UTC timestamp when the session was created.
    pub created_at: DateTime<Utc>,

    /// UTC timestamp when the session was approved or rejected.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl PairingSession {
    /// Create a new pairing session for `initiator_name`.
    ///
    /// Generates a fresh [`SyncKey`] and derives the corresponding
    /// [`PairingCode`].
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::InvalidPairingMaterial`] when `initiator_name` is
    /// empty.
    pub fn new(initiator_name: impl Into<String>) -> Result<Self, SyncError> {
        let initiator_name = initiator_name.into();
        if initiator_name.trim().is_empty() {
            return Err(SyncError::InvalidPairingMaterial(
                "initiator name must not be empty".into(),
            ));
        }
        let sync_key = SyncKey::generate();
        let pairing_code = sync_key.to_pairing_code();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            pairing_code,
            sync_key,
            initiator_name,
            state: ApprovalState::Pending,
            created_at: Utc::now(),
            resolved_at: None,
        })
    }

    /// Reconstruct a [`PairingSession`] on the receiving device from a
    /// known [`SyncKey`].
    ///
    /// # Errors
    ///
    /// Propagates [`SyncError::InvalidPairingMaterial`] from
    /// [`SyncKey::from_hex`] or when `initiator_name` is empty.
    pub fn from_key(
        hex_key: impl Into<String>,
        initiator_name: impl Into<String>,
    ) -> Result<Self, SyncError> {
        let initiator_name = initiator_name.into();
        if initiator_name.trim().is_empty() {
            return Err(SyncError::InvalidPairingMaterial(
                "initiator name must not be empty".into(),
            ));
        }
        let sync_key = SyncKey::from_hex(hex_key)?;
        let pairing_code = sync_key.to_pairing_code();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            pairing_code,
            sync_key,
            initiator_name,
            state: ApprovalState::Pending,
            created_at: Utc::now(),
            resolved_at: None,
        })
    }

    /// Approve the pairing session.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::InvalidPairingMaterial`] when the session is not
    /// in the `Pending` state.
    pub fn approve(&mut self) -> Result<(), SyncError> {
        if self.state != ApprovalState::Pending {
            return Err(SyncError::InvalidPairingMaterial(format!(
                "session {} is not pending (state: {:?})",
                self.id, self.state
            )));
        }
        self.state = ApprovalState::Approved;
        self.resolved_at = Some(Utc::now());
        Ok(())
    }

    /// Reject the pairing session.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::InvalidPairingMaterial`] when the session is not
    /// in the `Pending` state.
    pub fn reject(&mut self) -> Result<(), SyncError> {
        if self.state != ApprovalState::Pending {
            return Err(SyncError::InvalidPairingMaterial(format!(
                "session {} is not pending (state: {:?})",
                self.id, self.state
            )));
        }
        self.state = ApprovalState::Rejected;
        self.resolved_at = Some(Utc::now());
        Ok(())
    }

    /// Return `true` when the session has been approved.
    #[must_use]
    pub fn is_approved(&self) -> bool {
        self.state == ApprovalState::Approved
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_key_generate_produces_64_hex_chars() {
        let key = SyncKey::generate();
        assert_eq!(key.as_hex().len(), 64);
        assert!(key.as_hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sync_key_from_hex_accepts_valid_key() {
        let key = SyncKey::generate();
        let parsed = SyncKey::from_hex(key.as_hex()).unwrap();
        assert_eq!(parsed.as_hex(), key.as_hex());
    }

    #[test]
    fn sync_key_from_hex_rejects_short_string() {
        assert!(matches!(
            SyncKey::from_hex("abc"),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn sync_key_from_hex_rejects_non_hex() {
        let bad: String = "z".repeat(64);
        assert!(matches!(
            SyncKey::from_hex(bad),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn sync_key_from_hex_error_message_distinguishes_length_vs_non_hex() {
        // Wrong length → mentions "got N chars"
        let err = SyncKey::from_hex("abc").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("got 3 chars"), "unexpected message: {msg}");

        // Right length but non-hex → mentions "hexadecimal"
        let err = SyncKey::from_hex("z".repeat(64)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("hexadecimal"), "unexpected message: {msg}");
    }

    #[test]
    fn sync_key_from_hex_accepts_uppercase_input() {
        let key = SyncKey::generate();
        let upper = key.as_hex().to_uppercase();
        let parsed = SyncKey::from_hex(upper).unwrap();
        // Normalised to lowercase.
        assert_eq!(parsed.as_hex(), key.as_hex());
    }

    #[test]
    fn sync_key_display_is_redacted() {
        let key = SyncKey::generate();
        let display = format!("{key}");
        // Must not contain the full key value.
        assert!(
            !display.contains(key.as_hex()),
            "Display leaked the full key"
        );
        assert!(display.starts_with("SyncKey("));
    }

    #[test]
    fn sync_key_to_pairing_code_is_six_chars_uppercase() {
        let key = SyncKey::generate();
        let code = key.to_pairing_code();
        assert_eq!(code.as_str().len(), 6);
        assert!(code
            .as_str()
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    }

    #[test]
    fn pairing_session_new_rejects_empty_name() {
        assert!(matches!(
            PairingSession::new(""),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn pairing_session_new_creates_pending_session() {
        let session = PairingSession::new("Alice's MacBook").unwrap();
        assert_eq!(session.state, ApprovalState::Pending);
        assert!(!session.id.is_empty());
        assert!(session.resolved_at.is_none());
    }

    #[test]
    fn pairing_session_approve_transitions_to_approved() {
        let mut session = PairingSession::new("device-a").unwrap();
        session.approve().unwrap();
        assert!(session.is_approved());
        assert!(session.resolved_at.is_some());
    }

    #[test]
    fn pairing_session_reject_transitions_to_rejected() {
        let mut session = PairingSession::new("device-b").unwrap();
        session.reject().unwrap();
        assert_eq!(session.state, ApprovalState::Rejected);
        assert!(session.resolved_at.is_some());
    }

    #[test]
    fn pairing_session_double_approve_returns_error() {
        let mut session = PairingSession::new("device-c").unwrap();
        session.approve().unwrap();
        assert!(matches!(
            session.approve(),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn pairing_session_from_key_round_trips_sync_key() {
        let original = PairingSession::new("laptop").unwrap();
        let reconstructed = PairingSession::from_key(original.sync_key.as_hex(), "laptop").unwrap();
        assert_eq!(reconstructed.sync_key.as_hex(), original.sync_key.as_hex());
        assert_eq!(
            reconstructed.pairing_code.as_str(),
            original.pairing_code.as_str()
        );
    }

    #[test]
    fn sync_key_debug_masks_key() {
        let key = SyncKey::generate();
        let debug = format!("{key:?}");
        assert!(debug.starts_with("SyncKey("));
        assert!(debug.ends_with("...)"));
        // Must not contain the full key
        assert!(!debug.contains(key.as_hex()));
    }
}
