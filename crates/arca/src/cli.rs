//! CLI module — vault sub-commands for the `pares-agens` binary.
//!
//! Exposes the `vault` sub-command group:
//!
//! ```text
//! pares-agens vault lock
//! pares-agens vault unlock
//! pares-agens vault rotate
//! ```
//!
//! # Integration
//!
//! Integrate with a top-level `clap` CLI as:
//!
//! ```rust,no_run
//! use pares_agens_arca::cli::{VaultArgs, run_vault_cli};
//! use pares_agens_arca::vault::CredentialVault;
//!
//! // let args = VaultArgs::parse();  // from clap
//! // let mut vault: CredentialVault = /* load from persistent store */;
//! // run_vault_cli(&args, &mut vault).unwrap();
//! ```

use std::str::FromStr;

use crate::{vault::CredentialVault, ArcaError};

// ── VaultCommand ──────────────────────────────────────────────────────────────

/// Sub-commands available under the `vault` CLI group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultCommand {
    /// Lock the vault, wiping the in-memory DEK.
    Lock,
    /// Unlock the vault with the master password.
    Unlock,
    /// Rotate the master password (re-wraps the DEK, no secret re-encryption).
    Rotate,
}

/// Error returned when an unknown vault command string is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub String);

impl std::fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown vault command: '{}'", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

impl FromStr for VaultCommand {
    type Err = UnknownCommand;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "lock" => Ok(Self::Lock),
            "unlock" => Ok(Self::Unlock),
            "rotate" => Ok(Self::Rotate),
            _ => Err(UnknownCommand(s.to_owned())),
        }
    }
}

impl VaultCommand {
    /// Return a human-readable name for the command.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::Rotate => "rotate",
        }
    }
}

// ── VaultArgs ─────────────────────────────────────────────────────────────────

/// Arguments for the `vault` CLI group.
///
/// Designed for use with `clap` but kept free from `clap` annotations so that
/// the `pares-agens-arca` crate does not pull in a `clap` dependency.  The
/// integration crate (`pares-agens-service` or the Tauri binary) is responsible
/// for parsing these from the command line.
#[derive(Debug, Clone)]
pub struct VaultArgs {
    /// Which vault operation to perform.
    pub command: VaultCommand,

    /// Master password used for `unlock` and `rotate`.
    ///
    /// For `rotate`, this is the **new** master password.  The vault must
    /// already be unlocked before calling `rotate`.
    pub password: Option<String>,
}

impl VaultArgs {
    /// Create args for `lock` (no password needed).
    pub fn lock() -> Self {
        Self {
            command: VaultCommand::Lock,
            password: None,
        }
    }

    /// Create args for `unlock` with the given password.
    pub fn unlock(password: impl Into<String>) -> Self {
        Self {
            command: VaultCommand::Unlock,
            password: Some(password.into()),
        }
    }

    /// Create args for `rotate` with the given new password.
    pub fn rotate(new_password: impl Into<String>) -> Self {
        Self {
            command: VaultCommand::Rotate,
            password: Some(new_password.into()),
        }
    }
}

// ── run_vault_cli ─────────────────────────────────────────────────────────────

/// Dispatch a vault CLI command against `vault`.
///
/// Writes a human-readable result to `stdout` on success.
///
/// # Errors
///
/// Returns [`ArcaError`] if the operation fails (e.g. wrong password, vault
/// not initialised, missing password argument).
pub fn run_vault_cli(args: &VaultArgs, vault: &mut CredentialVault) -> Result<(), ArcaError> {
    match &args.command {
        VaultCommand::Lock => {
            vault.lock();
            println!("✓ Vault locked.");
        }
        VaultCommand::Unlock => {
            let password = args
                .password
                .as_deref()
                .ok_or_else(|| ArcaError::CryptoError("unlock requires a password".to_string()))?;
            vault.unlock(password)?;
            println!("✓ Vault unlocked.");
        }
        VaultCommand::Rotate => {
            let new_password = args.password.as_deref().ok_or_else(|| {
                ArcaError::CryptoError("rotate requires a new password".to_string())
            })?;
            vault.rotate_key(new_password)?;
            println!("✓ Key rotated. The vault remains unlocked.");
        }
    }
    Ok(())
}

/// Print usage information for the vault CLI group.
pub fn print_usage() {
    println!("Usage: pares-agens vault <COMMAND>");
    println!();
    println!("Commands:");
    println!("  lock     Lock the vault (wipes the in-memory decryption key)");
    println!("  unlock   Unlock the vault with the master password");
    println!("  rotate   Rotate the master password (vault must be unlocked first)");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::CredentialVault;

    fn init_vault(password: &str) -> CredentialVault {
        let mut v = CredentialVault::new(None);
        v.initialise(password).unwrap();
        v
    }

    #[test]
    fn from_str_parses_all_commands() {
        assert!(matches!(
            "lock".parse::<VaultCommand>(),
            Ok(VaultCommand::Lock)
        ));
        assert!(matches!(
            "unlock".parse::<VaultCommand>(),
            Ok(VaultCommand::Unlock)
        ));
        assert!(matches!(
            "rotate".parse::<VaultCommand>(),
            Ok(VaultCommand::Rotate)
        ));
    }

    #[test]
    fn from_str_case_insensitive() {
        assert!(matches!(
            "LOCK".parse::<VaultCommand>(),
            Ok(VaultCommand::Lock)
        ));
        assert!(matches!(
            "Unlock".parse::<VaultCommand>(),
            Ok(VaultCommand::Unlock)
        ));
        assert!(matches!(
            "ROTATE".parse::<VaultCommand>(),
            Ok(VaultCommand::Rotate)
        ));
    }

    #[test]
    fn from_str_returns_err_for_unknown() {
        assert!("open".parse::<VaultCommand>().is_err());
        assert!("".parse::<VaultCommand>().is_err());
    }

    #[test]
    fn name_matches_from_str_input() {
        let commands = [
            VaultCommand::Lock,
            VaultCommand::Unlock,
            VaultCommand::Rotate,
        ];
        for cmd in &commands {
            let parsed = cmd.name().parse::<VaultCommand>();
            assert!(parsed.is_ok(), "parse({}) should succeed", cmd.name());
        }
    }

    #[test]
    fn run_cli_lock_succeeds() {
        let mut vault = init_vault("pw");
        assert!(vault.is_unlocked());
        let result = run_vault_cli(&VaultArgs::lock(), &mut vault);
        assert!(result.is_ok());
        assert!(!vault.is_unlocked());
    }

    #[test]
    fn run_cli_unlock_succeeds() {
        let mut vault = init_vault("pw");
        vault.lock();
        let result = run_vault_cli(&VaultArgs::unlock("pw"), &mut vault);
        assert!(result.is_ok());
        assert!(vault.is_unlocked());
    }

    #[test]
    fn run_cli_unlock_wrong_password_fails() {
        let mut vault = init_vault("pw");
        vault.lock();
        let result = run_vault_cli(&VaultArgs::unlock("wrong"), &mut vault);
        assert!(matches!(result, Err(ArcaError::CryptoError(_))));
    }

    #[test]
    fn run_cli_unlock_no_password_fails() {
        let mut vault = init_vault("pw");
        vault.lock();
        let args = VaultArgs {
            command: VaultCommand::Unlock,
            password: None,
        };
        let result = run_vault_cli(&args, &mut vault);
        assert!(matches!(result, Err(ArcaError::CryptoError(_))));
    }

    #[test]
    fn run_cli_rotate_succeeds_and_old_password_fails() {
        let mut vault = init_vault("old-pw");
        vault.store_credential("key", "value", None).unwrap();
        let result = run_vault_cli(&VaultArgs::rotate("new-pw"), &mut vault);
        assert!(result.is_ok());
        // Data still accessible after rotation.
        assert_eq!(vault.retrieve_credential("key").unwrap(), "value");
        // Old password no longer works after lock/unlock cycle.
        vault.lock();
        assert!(vault.unlock("old-pw").is_err());
        assert!(vault.unlock("new-pw").is_ok());
    }

    #[test]
    fn run_cli_rotate_no_password_fails() {
        let mut vault = init_vault("pw");
        let args = VaultArgs {
            command: VaultCommand::Rotate,
            password: None,
        };
        let result = run_vault_cli(&args, &mut vault);
        assert!(matches!(result, Err(ArcaError::CryptoError(_))));
    }

    #[test]
    fn run_cli_rotate_when_locked_fails() {
        let mut vault = init_vault("pw");
        vault.lock();
        let result = run_vault_cli(&VaultArgs::rotate("new-pw"), &mut vault);
        assert!(matches!(result, Err(ArcaError::VaultLocked)));
    }
}
