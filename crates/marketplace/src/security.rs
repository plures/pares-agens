//! Security validation for third-party marketplace skills and extensions.
//!
//! [`SecurityChecker`] provides three layers of defence:
//!
//! 1. **Checksum verification** — confirms the downloaded archive matches the
//!    SHA-256 digest published in [`SkillMetadata`].
//! 2. **Signature verification** — validates that the optional detached
//!    signature in [`SkillMetadata`] is present and non-empty when required.
//! 3. **Pattern scan** — inspects an installed skill directory for known
//!    suspicious patterns (e.g. hard-coded credentials, shell-injection
//!    markers, obfuscated payloads).
//!
//! A [`SecurityReport`] aggregates all findings and provides a convenient
//! [`is_safe`](SecurityReport::is_safe) predicate.
//!
//! Checksum verification computes a real SHA-256 digest of the supplied bytes
//! (via the `sha2` crate) and compares it against the expected hex value.
//! Signature verification checks that a signature field is present and
//! non-empty.  Pattern scanning uses a keyword list rather than a full
//! static-analysis pass.

use crate::{MarketplaceError, SkillMetadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── Severity ──────────────────────────────────────────────────────────────────

/// Severity rating of a security finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// Informational — no immediate risk.
    Low,
    /// Warrants attention but not necessarily blocking.
    Medium,
    /// Should block installation unless explicitly overridden.
    High,
    /// Must block installation; potential for serious harm.
    Critical,
}

// ── ViolationKind ─────────────────────────────────────────────────────────────

/// The kind of security issue that was detected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViolationKind {
    /// The archive checksum does not match the value in the metadata.
    InvalidChecksum,
    /// The required detached signature is absent or empty.
    InvalidSignature,
    /// A suspicious pattern (hard-coded credential, shell injection, etc.)
    /// was found in the skill's source files.
    SuspiciousPattern,
    /// Required metadata fields are absent.
    MissingMetadata,
}

// ── SecurityViolation ─────────────────────────────────────────────────────────

/// A single security finding within a skill or extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityViolation {
    /// Classification of this finding.
    pub kind: ViolationKind,

    /// Human-readable description of the problem.
    pub description: String,

    /// Severity rating.
    pub severity: Severity,
}

// ── SecurityReport ────────────────────────────────────────────────────────────

/// Aggregated security findings for a single skill or extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityReport {
    /// All violations found during the security check.
    pub violations: Vec<SecurityViolation>,
}

impl SecurityReport {
    /// Return `true` when no violations were found (the skill is considered safe).
    #[must_use]
    pub fn is_safe(&self) -> bool {
        self.violations.is_empty()
    }

    /// Return `true` when at least one violation with severity ≥ `min_severity`
    /// was found.
    #[must_use]
    pub fn has_severity_at_least(&self, min_severity: &Severity) -> bool {
        self.violations.iter().any(|v| &v.severity >= min_severity)
    }
}

// ── SecurityChecker ───────────────────────────────────────────────────────────

/// Performs multi-layer security validation for marketplace skills.
#[derive(Debug, Default)]
pub struct SecurityChecker;

impl SecurityChecker {
    /// Create a new `SecurityChecker`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Verify that `data` matches `expected_checksum` (a lowercase SHA-256 hex
    /// digest).
    ///
    /// The digest is computed with a real SHA-256 implementation (`sha2`).
    ///
    /// # Errors
    ///
    /// - [`MarketplaceError::InvalidMetadata`] — `expected_checksum` is not a
    ///   64-character hex string.
    /// - [`MarketplaceError::SecurityViolation`] — the computed digest does
    ///   not match `expected_checksum`.
    pub fn verify_checksum(
        &self,
        data: &[u8],
        expected_checksum: &str,
    ) -> Result<(), MarketplaceError> {
        if expected_checksum.len() != 64
            || !expected_checksum.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "expected_checksum '{}' is not a valid SHA-256 hex digest",
                expected_checksum
            )));
        }

        let computed = sha256_hex(data);
        if computed != expected_checksum {
            return Err(MarketplaceError::SecurityViolation(format!(
                "checksum mismatch: expected {expected_checksum}, computed {computed}"
            )));
        }
        Ok(())
    }

    /// Verify that `metadata` carries a non-empty signature when one is
    /// required.
    ///
    /// A signature is considered present when `metadata.signature` is
    /// `Some(s)` and `s` is non-empty.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::SecurityViolation`] when the signature
    /// field is absent or empty.
    pub fn verify_signature(&self, metadata: &SkillMetadata) -> Result<(), MarketplaceError> {
        match &metadata.signature {
            Some(sig) if !sig.is_empty() => Ok(()),
            _ => Err(MarketplaceError::SecurityViolation(format!(
                "skill '{}' is missing a valid signature",
                metadata.id
            ))),
        }
    }

    /// Scan `content` (the text of a skill source file or manifest) for known
    /// suspicious patterns.
    ///
    /// Returns a [`SecurityReport`] listing any violations found.  A clean
    /// scan yields an empty report ([`SecurityReport::is_safe`] returns
    /// `true`).
    ///
    /// Patterns checked:
    ///
    /// | Pattern keyword        | Violation kind        | Severity |
    /// |------------------------|-----------------------|----------|
    /// | `eval(`                | SuspiciousPattern     | High     |
    /// | `exec(`                | SuspiciousPattern     | High     |
    /// | `__import__`           | SuspiciousPattern     | High     |
    /// | `subprocess`           | SuspiciousPattern     | Medium   |
    /// | `os.system`            | SuspiciousPattern     | High     |
    /// | `base64.decode`        | SuspiciousPattern     | Medium   |
    /// | `password`             | SuspiciousPattern     | Medium   |
    /// | `secret`               | SuspiciousPattern     | Medium   |
    /// | `api_key`              | SuspiciousPattern     | Medium   |
    #[must_use]
    pub fn scan_content(&self, content: &str) -> SecurityReport {
        let lower = content.to_lowercase();
        let mut violations = Vec::new();

        for (pattern, description, severity) in SUSPICIOUS_PATTERNS {
            if lower.contains(pattern) {
                violations.push(SecurityViolation {
                    kind: ViolationKind::SuspiciousPattern,
                    description: format!(
                        "suspicious pattern detected: '{pattern}' — {description}"
                    ),
                    severity: severity.clone(),
                });
            }
        }

        SecurityReport { violations }
    }

    /// Run all security checks against `metadata` and `content`, returning a
    /// consolidated [`SecurityReport`].
    ///
    /// Checks performed:
    /// 1. Metadata completeness (`id`, `checksum`, `download_url`).
    /// 2. Content pattern scan via [`scan_content`](Self::scan_content).
    #[must_use]
    pub fn full_check(&self, metadata: &SkillMetadata, content: &str) -> SecurityReport {
        let mut violations = Vec::new();

        // Metadata completeness checks.
        if metadata.id.is_empty() {
            violations.push(SecurityViolation {
                kind: ViolationKind::MissingMetadata,
                description: "skill id is missing".to_string(),
                severity: Severity::Critical,
            });
        }
        if metadata.checksum.is_empty() {
            violations.push(SecurityViolation {
                kind: ViolationKind::MissingMetadata,
                description: "checksum is missing".to_string(),
                severity: Severity::Critical,
            });
        }
        if metadata.download_url.is_empty() {
            violations.push(SecurityViolation {
                kind: ViolationKind::MissingMetadata,
                description: "download_url is missing".to_string(),
                severity: Severity::High,
            });
        }

        // Content pattern scan.
        let content_report = self.scan_content(content);
        violations.extend(content_report.violations);

        SecurityReport { violations }
    }
}

// ── Suspicious pattern table ──────────────────────────────────────────────────

/// `(pattern, description, severity)` tuples for the content scanner.
const SUSPICIOUS_PATTERNS: &[(&str, &str, Severity)] = &[
    ("eval(", "dynamic code execution", Severity::High),
    ("exec(", "dynamic code execution", Severity::High),
    ("__import__", "dynamic module import", Severity::High),
    (
        "subprocess",
        "shell subprocess invocation",
        Severity::Medium,
    ),
    (
        "os.system",
        "direct shell command execution",
        Severity::High,
    ),
    (
        "base64.decode",
        "potentially obfuscated payload",
        Severity::Medium,
    ),
    (
        "password",
        "hard-coded credential candidate",
        Severity::Medium,
    ),
    ("secret", "hard-coded secret candidate", Severity::Medium),
    ("api_key", "hard-coded API key candidate", Severity::Medium),
];

// ── SHA-256 ───────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;
    let hash = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in &hash {
        write!(s, "{b:02x}").expect("formatting to String is infallible");
    }
    s
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::valid_metadata;

    // ── verify_checksum ───────────────────────────────────────────────────────

    #[test]
    fn verify_checksum_accepts_matching_digest() {
        let checker = SecurityChecker::new();
        let data = b"hello world";
        let digest = sha256_hex(data);
        assert!(checker.verify_checksum(data, &digest).is_ok());
    }

    #[test]
    fn verify_checksum_rejects_wrong_digest() {
        let checker = SecurityChecker::new();
        let data = b"hello world";
        let wrong = "a".repeat(64);
        assert!(matches!(
            checker.verify_checksum(data, &wrong),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    #[test]
    fn verify_checksum_rejects_invalid_hex_string() {
        let checker = SecurityChecker::new();
        assert!(matches!(
            checker.verify_checksum(b"data", "not-a-hash"),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    /// Known-good SHA-256 fixture: SHA-256(b"hello world") verified against
    /// the system `sha256sum` utility.
    #[test]
    fn verify_checksum_known_good_hello_world() {
        let checker = SecurityChecker::new();
        // echo -n "hello world" | sha256sum
        let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(checker.verify_checksum(b"hello world", digest).is_ok());
    }

    /// Known-good SHA-256 fixture: SHA-256(b"") is the well-known empty hash.
    #[test]
    fn verify_checksum_known_good_empty() {
        let checker = SecurityChecker::new();
        // SHA-256 of the empty string — RFC-4634 test vector.
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(checker.verify_checksum(b"", digest).is_ok());
    }

    /// SHA-256 of one input must not match a different input.
    #[test]
    fn verify_checksum_different_data_fails() {
        let checker = SecurityChecker::new();
        let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(matches!(
            checker.verify_checksum(b"hello WORLD", digest),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    // ── verify_signature ──────────────────────────────────────────────────────

    #[test]
    fn verify_signature_accepts_non_empty_signature() {
        let checker = SecurityChecker::new();
        let mut m = valid_metadata();
        m.signature = Some("c2lnbmF0dXJl".to_string());
        assert!(checker.verify_signature(&m).is_ok());
    }

    #[test]
    fn verify_signature_rejects_missing_signature() {
        let checker = SecurityChecker::new();
        let mut m = valid_metadata();
        m.signature = None;
        assert!(matches!(
            checker.verify_signature(&m),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    #[test]
    fn verify_signature_rejects_empty_signature() {
        let checker = SecurityChecker::new();
        let mut m = valid_metadata();
        m.signature = Some(String::new());
        assert!(matches!(
            checker.verify_signature(&m),
            Err(MarketplaceError::SecurityViolation(_))
        ));
    }

    // ── scan_content ──────────────────────────────────────────────────────────

    #[test]
    fn scan_content_clean_file_is_safe() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("fn hello() { println!(\"Hello, world!\"); }");
        assert!(report.is_safe());
    }

    #[test]
    fn scan_content_detects_eval() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("result = eval(user_input)");
        assert!(!report.is_safe());
        assert!(report
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::SuspiciousPattern));
    }

    #[test]
    fn scan_content_detects_hardcoded_password() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("let password = \"hunter2\";");
        assert!(!report.is_safe());
    }

    #[test]
    fn scan_content_is_case_insensitive() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("EVAL(something)");
        assert!(!report.is_safe());
    }

    #[test]
    fn scan_content_detects_multiple_violations() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("eval(x); api_key = \"abc\"");
        assert!(report.violations.len() >= 2);
    }

    #[test]
    fn scan_content_has_severity_at_least_works() {
        let checker = SecurityChecker::new();
        let report = checker.scan_content("eval(x)");
        assert!(report.has_severity_at_least(&Severity::High));
        assert!(!report.has_severity_at_least(&Severity::Critical));
    }

    // ── full_check ────────────────────────────────────────────────────────────

    #[test]
    fn full_check_clean_metadata_and_content_is_safe() {
        let checker = SecurityChecker::new();
        let m = valid_metadata();
        let report = checker.full_check(&m, "fn greet() {}");
        assert!(report.is_safe());
    }

    #[test]
    fn full_check_reports_missing_metadata() {
        let checker = SecurityChecker::new();
        let mut m = valid_metadata();
        m.id = String::new();
        let report = checker.full_check(&m, "fn greet() {}");
        assert!(!report.is_safe());
        assert!(report
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::MissingMetadata));
    }

    #[test]
    fn full_check_aggregates_content_violations() {
        let checker = SecurityChecker::new();
        let m = valid_metadata();
        let report = checker.full_check(&m, "eval(user_input)");
        assert!(!report.is_safe());
        assert!(report
            .violations
            .iter()
            .any(|v| v.kind == ViolationKind::SuspiciousPattern));
    }
}
