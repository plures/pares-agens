//! CLI module — marketplace sub-commands for the `pares-agens` binary.
//!
//! Exposes a thin command-dispatch layer for the marketplace sub-command group:
//!
//! ```text
//! pares-agens marketplace search  <query>
//! pares-agens marketplace install <id>
//! pares-agens marketplace update  [id]
//! pares-agens marketplace remove  <id>
//! pares-agens marketplace list
//! ```

use crate::{
    discovery::MarketplaceClient, installer::Installer, update::UpdateChecker, MarketplaceError,
};
use std::str::FromStr;

// ── MarketplaceCommand ────────────────────────────────────────────────────────

/// Sub-commands available under the `marketplace` CLI group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketplaceCommand {
    /// Search the remote catalogue for procedures matching a query string.
    Search(String),
    /// Download and install a procedure by its marketplace id.
    Install(String),
    /// Check for and apply updates.  When an id is supplied only that skill is
    /// updated; otherwise all installed skills are checked.
    Update(Option<String>),
    /// Uninstall a procedure by its marketplace id.
    Remove(String),
    /// List locally installed procedures.
    List,
}

/// Error returned when an unknown or malformed marketplace command is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub String);

impl std::fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown marketplace command: '{}'", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

impl FromStr for MarketplaceCommand {
    type Err = UnknownCommand;

    /// Parse a command verb from a string slice.
    ///
    /// Only the command verb is parsed here; arguments are expected to be
    /// provided separately.  Use [`MarketplaceCommand::parse_with_args`] to
    /// parse a full `(verb, args)` pair.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "list" => Ok(Self::List),
            _ => Err(UnknownCommand(s.to_owned())),
        }
    }
}

impl MarketplaceCommand {
    /// Parse a command verb together with its argument(s).
    ///
    /// # Errors
    ///
    /// Returns [`UnknownCommand`] when the verb is unrecognised, or when a
    /// required argument is missing.
    pub fn parse_with_args(verb: &str, args: &[&str]) -> Result<Self, UnknownCommand> {
        match verb.to_ascii_lowercase().as_str() {
            "search" => {
                let query = args.first().ok_or_else(|| {
                    UnknownCommand("search requires a query argument".to_string())
                })?;
                Ok(Self::Search((*query).to_string()))
            }
            "install" => {
                let id = args.first().ok_or_else(|| {
                    UnknownCommand("install requires a procedure id argument".to_string())
                })?;
                Ok(Self::Install((*id).to_string()))
            }
            "update" => Ok(Self::Update(args.first().map(|s| (*s).to_string()))),
            "remove" => {
                let id = args.first().ok_or_else(|| {
                    UnknownCommand("remove requires a procedure id argument".to_string())
                })?;
                Ok(Self::Remove((*id).to_string()))
            }
            "list" => Ok(Self::List),
            _ => Err(UnknownCommand(verb.to_owned())),
        }
    }

    /// Return a human-readable name for the command verb.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Search(_) => "search",
            Self::Install(_) => "install",
            Self::Update(_) => "update",
            Self::Remove(_) => "remove",
            Self::List => "list",
        }
    }
}

// ── CLI runner ────────────────────────────────────────────────────────────────

/// Dispatch a [`MarketplaceCommand`] against the provided `client`,
/// `installer`, and `checker`.
///
/// Writes a human-readable result to `stdout` and returns any error that
/// occurred during execution.
pub fn run_cli(
    cmd: &MarketplaceCommand,
    client: &MarketplaceClient,
    installer: &mut Installer,
    checker: &UpdateChecker,
) -> Result<(), MarketplaceError> {
    match cmd {
        MarketplaceCommand::Search(query) => {
            let results = client.search(query, None)?;
            if results.is_empty() {
                println!("No procedures found matching '{query}'.");
            } else {
                println!("Found {} procedure(s) matching '{query}':", results.len());
                for skill in &results {
                    println!("  {} ({}) — {}", skill.id, skill.version, skill.description);
                }
            }
        }

        MarketplaceCommand::Install(id) => {
            let metadata = client.get_skill(id)?;
            let installed = installer.install(metadata)?;
            println!(
                "✓ Installed '{}' {} to {}.",
                installed.metadata.id, installed.metadata.version, installed.install_path
            );
        }

        MarketplaceCommand::Update(filter_id) => {
            let all_installed = installer.list_installed();
            let updates = checker.check_updates(all_installed)?;

            let applicable: Vec<_> = match filter_id {
                Some(id) => updates.iter().filter(|u| &u.skill_id == id).collect(),
                None => updates.iter().collect(),
            };

            if applicable.is_empty() {
                println!("All procedures are up to date.");
            } else {
                println!("{} update(s) available:", applicable.len());
                for upd in &applicable {
                    println!(
                        "  {} {} → {}",
                        upd.skill_id, upd.installed_version, upd.available_version
                    );
                    // Re-install with the remote metadata when available.
                    if let Some(remote_meta) = checker.remote_metadata(&upd.skill_id) {
                        // Remove old version first, then install new one.
                        let _ = installer.uninstall(&upd.skill_id);
                        match installer.install(remote_meta.clone()) {
                            Ok(installed) => println!(
                                "  ✓ Updated to {} at {}.",
                                installed.metadata.version, installed.install_path
                            ),
                            Err(e) => println!("  ✗ Update failed: {e}"),
                        }
                    }
                }
            }
        }

        MarketplaceCommand::Remove(id) => {
            installer.uninstall(id)?;
            println!("✓ Removed '{id}'.");
        }

        MarketplaceCommand::List => {
            let installed = installer.list_installed();
            if installed.is_empty() {
                println!("No procedures currently installed.");
            } else {
                println!("{} procedure(s) installed:", installed.len());
                for skill in installed {
                    let last_used = skill.last_used.as_deref().unwrap_or("never");
                    println!(
                        "  {} ({}) — installed {} — last used {}",
                        skill.metadata.id, skill.metadata.version, skill.installed_at, last_used,
                    );
                }
            }
        }
    }
    Ok(())
}

/// Print usage information for the marketplace CLI group.
pub fn print_usage() {
    println!("Usage: pares-agens marketplace <COMMAND> [ARGS]");
    println!();
    println!("Commands:");
    println!("  search <query>   Search the marketplace catalogue");
    println!("  install <id>     Install a procedure by id");
    println!("  update [id]      Update installed procedures (all or a specific one)");
    println!("  remove <id>      Uninstall a procedure by id");
    println!("  list             List installed procedures");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SkillCategory, SkillMetadata};

    fn make_skill(id: &str, version: &str) -> SkillMetadata {
        SkillMetadata {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            description: "A test procedure.".to_string(),
            author: "pares".to_string(),
            categories: vec![SkillCategory::Coding("rust".to_string())],
            checksum: "a".repeat(64),
            download_url: format!("https://marketplace.example.com/{id}.tar.gz"),
            signature: None,
        }
    }

    fn make_client() -> MarketplaceClient {
        MarketplaceClient::new("https://marketplace.example.com")
            .with_catalogue(vec![make_skill("pares/rust-helper", "1.0.0")])
    }

    fn make_installer() -> Installer {
        Installer::new("/skills").unwrap()
    }

    fn make_checker() -> UpdateChecker {
        UpdateChecker::new()
    }

    // ── MarketplaceCommand::parse_with_args ───────────────────────────────────

    #[test]
    fn parse_search_with_query() {
        let cmd = MarketplaceCommand::parse_with_args("search", &["rust"]).unwrap();
        assert_eq!(cmd, MarketplaceCommand::Search("rust".to_string()));
    }

    #[test]
    fn parse_search_requires_query() {
        assert!(MarketplaceCommand::parse_with_args("search", &[]).is_err());
    }

    #[test]
    fn parse_install_with_id() {
        let cmd = MarketplaceCommand::parse_with_args("install", &["pares/rust-helper"]).unwrap();
        assert_eq!(
            cmd,
            MarketplaceCommand::Install("pares/rust-helper".to_string())
        );
    }

    #[test]
    fn parse_install_requires_id() {
        assert!(MarketplaceCommand::parse_with_args("install", &[]).is_err());
    }

    #[test]
    fn parse_update_without_id() {
        let cmd = MarketplaceCommand::parse_with_args("update", &[]).unwrap();
        assert_eq!(cmd, MarketplaceCommand::Update(None));
    }

    #[test]
    fn parse_update_with_id() {
        let cmd = MarketplaceCommand::parse_with_args("update", &["pares/rust-helper"]).unwrap();
        assert_eq!(
            cmd,
            MarketplaceCommand::Update(Some("pares/rust-helper".to_string()))
        );
    }

    #[test]
    fn parse_remove_with_id() {
        let cmd = MarketplaceCommand::parse_with_args("remove", &["pares/rust-helper"]).unwrap();
        assert_eq!(
            cmd,
            MarketplaceCommand::Remove("pares/rust-helper".to_string())
        );
    }

    #[test]
    fn parse_remove_requires_id() {
        assert!(MarketplaceCommand::parse_with_args("remove", &[]).is_err());
    }

    #[test]
    fn parse_list() {
        let cmd = MarketplaceCommand::parse_with_args("list", &[]).unwrap();
        assert_eq!(cmd, MarketplaceCommand::List);
    }

    #[test]
    fn parse_is_case_insensitive() {
        let cmd = MarketplaceCommand::parse_with_args("SEARCH", &["rust"]).unwrap();
        assert_eq!(cmd, MarketplaceCommand::Search("rust".to_string()));
    }

    #[test]
    fn parse_unknown_verb_returns_err() {
        assert!(MarketplaceCommand::parse_with_args("deploy", &[]).is_err());
    }

    // ── run_cli: search ───────────────────────────────────────────────────────

    #[test]
    fn run_cli_search_returns_ok_with_results() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Search("rust".to_string());
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
    }

    #[test]
    fn run_cli_search_returns_ok_with_no_results() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Search("nonexistent".to_string());
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
    }

    #[test]
    fn run_cli_search_propagates_empty_query_error() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Search(String::new());
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_err());
    }

    // ── run_cli: install ──────────────────────────────────────────────────────

    #[test]
    fn run_cli_install_succeeds_for_known_id() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Install("pares/rust-helper".to_string());
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
        assert!(installer.is_installed("pares/rust-helper"));
    }

    #[test]
    fn run_cli_install_fails_for_unknown_id() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Install("unknown/skill".to_string());
        assert!(matches!(
            run_cli(&cmd, &client, &mut installer, &checker),
            Err(MarketplaceError::NotFound(_))
        ));
    }

    // ── run_cli: remove ───────────────────────────────────────────────────────

    #[test]
    fn run_cli_remove_succeeds_when_installed() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        // Install first.
        installer
            .install(make_skill("pares/rust-helper", "1.0.0"))
            .unwrap();
        let cmd = MarketplaceCommand::Remove("pares/rust-helper".to_string());
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
        assert!(!installer.is_installed("pares/rust-helper"));
    }

    #[test]
    fn run_cli_remove_fails_when_not_installed() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::Remove("pares/rust-helper".to_string());
        assert!(matches!(
            run_cli(&cmd, &client, &mut installer, &checker),
            Err(MarketplaceError::NotFound(_))
        ));
    }

    // ── run_cli: list ─────────────────────────────────────────────────────────

    #[test]
    fn run_cli_list_succeeds_when_empty() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        let cmd = MarketplaceCommand::List;
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
    }

    #[test]
    fn run_cli_list_succeeds_with_installed_procedures() {
        let client = make_client();
        let mut installer = make_installer();
        let checker = make_checker();
        installer
            .install(make_skill("pares/rust-helper", "1.0.0"))
            .unwrap();
        let cmd = MarketplaceCommand::List;
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
    }

    // ── run_cli: update ───────────────────────────────────────────────────────

    #[test]
    fn run_cli_update_reports_all_up_to_date() {
        let client = make_client();
        let mut installer = make_installer();
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.0")]);
        installer
            .install(make_skill("pares/rust-helper", "1.0.0"))
            .unwrap();
        let cmd = MarketplaceCommand::Update(None);
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
    }

    #[test]
    fn run_cli_update_applies_available_update() {
        let client = make_client();
        let mut installer = make_installer();
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.1")]);
        installer
            .install(make_skill("pares/rust-helper", "1.0.0"))
            .unwrap();
        let cmd = MarketplaceCommand::Update(None);
        assert!(run_cli(&cmd, &client, &mut installer, &checker).is_ok());
        // After update, 1.0.1 should be installed.
        assert!(installer.is_installed("pares/rust-helper"));
    }
}
