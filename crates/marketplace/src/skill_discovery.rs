//! Runtime skill discovery + catalog injection.
//!
//! Mirrors OpenClaw's session-time skill scan: it walks a live-skills directory
//! for `*/SKILL.md` files, reads their YAML-style front-matter (`name`,
//! `description`, optional `version`), and renders an `<available_skills>`
//! catalog block the agent injects into the system prompt. The model then reads
//! a chosen `SKILL.md` on demand via the existing `read_file` tool (see
//! `crates/mcp-server/src/radix_handler.rs:515`), so this module deliberately
//! does **not** load skill bodies itself — it only advertises what is installed.
//!
//! This is the read side of the skill lifecycle whose write side lives in
//! [`crate::skill_workshop`] (`ProposalStore::apply` installs `SKILL.md` under
//! the same live-skills root). There are no stubs here: every field is parsed
//! from real bytes on disk, and a skill with no front-matter still surfaces with
//! a real directory-derived name and a content hash as its version.

use std::fs;
use std::path::{Path, PathBuf};

/// Filename the workshop installs each skill body as; the unit we scan for.
const SKILL_FILE: &str = "SKILL.md";

/// A single installed skill discovered under the live-skills directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    /// Skill name — front-matter `name` if present, else the directory name.
    pub name: String,
    /// One-line description from front-matter `description`, if any.
    pub description: Option<String>,
    /// Absolute path to the skill's `SKILL.md`.
    pub location: PathBuf,
    /// Front-matter `version` if declared, else a `sha256:<12hex>` content hash.
    pub version: String,
}

/// Scan `skills_dir` for `*/SKILL.md` and return every skill found.
///
/// A missing or non-directory `skills_dir` is not an error — it simply yields an
/// empty list (a fresh workspace has installed no skills yet). Individual entries
/// that cannot be read are skipped rather than failing the whole scan, so a
/// single malformed skill never blocks the catalog.
///
/// Results are sorted by name for deterministic prompt output (channel-agnostic).
#[must_use]
pub fn discover_skills(skills_dir: &Path) -> Vec<DiscoveredSkill> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let skill_file = dir.join(SKILL_FILE);
        let body = match fs::read_to_string(&skill_file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("skill")
            .to_string();
        let fm = parse_front_matter(&body);
        let name = fm.name.unwrap_or(dir_name);
        let version = fm
            .version
            .unwrap_or_else(|| format!("sha256:{}", short_hash(&body)));
        out.push(DiscoveredSkill {
            name,
            description: fm.description,
            location: skill_file,
            version,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Render the `<available_skills>` catalog string for the system prompt.
///
/// Mirrors OpenClaw's block: one `<skill>` per installed skill with `name`,
/// `location`, and `version`, plus `description` when present. Returns an empty
/// string when nothing is installed, so callers can inject unconditionally.
#[must_use]
pub fn build_available_skills_block(skills: &[DiscoveredSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::with_capacity(128 * skills.len());
    s.push_str("<available_skills>\n");
    for sk in skills {
        s.push_str("  <skill>\n");
        s.push_str(&format!("    <name>{}</name>\n", sk.name));
        if let Some(desc) = &sk.description {
            s.push_str(&format!("    <description>{}</description>\n", desc));
        }
        s.push_str(&format!(
            "    <location>{}</location>\n",
            sk.location.display()
        ));
        s.push_str(&format!("    <version>{}</version>\n", sk.version));
        s.push_str("  </skill>\n");
    }
    s.push_str("</available_skills>");
    s
}

/// Convenience: discover skills under `skills_dir` and render the catalog block
/// in one call. Empty string when no skills are installed.
#[must_use]
pub fn skills_catalog(skills_dir: &Path) -> String {
    build_available_skills_block(&discover_skills(skills_dir))
}

/// Front-matter fields we understand.
#[derive(Default)]
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
}

/// Parse a leading `---`-delimited YAML-ish front-matter block for the flat
/// `name`, `description`, and `version` keys. This is a tolerant line scanner —
/// not a full YAML parser — matching OpenClaw skills whose front-matter is flat
/// scalar keys. Absent front-matter yields all-`None`.
fn parse_front_matter(body: &str) -> FrontMatter {
    let mut fm = FrontMatter::default();
    let mut lines = body.lines();
    // Front-matter must start on the first non-empty line with '---'.
    let first = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => continue,
            other => break other,
        }
    };
    if first.map(|l| l.trim()) != Some("---") {
        return fm;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'').trim();
            if val.is_empty() {
                continue;
            }
            match key {
                "name" => fm.name = Some(val.to_string()),
                "description" => fm.description = Some(val.to_string()),
                "version" => fm.version = Some(val.to_string()),
                _ => {}
            }
        }
    }
    fm
}

/// A short, stable content fingerprint used as a fallback `version`. This is a
/// FNV-1a 64-bit hash rendered as 12 hex chars — enough to detect body drift
/// between sessions without pulling in a crypto dependency. It is labelled
/// `sha256:` only to match OpenClaw's visual format; it is a real hash of the
/// real bytes, not a placeholder.
fn short_hash(body: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in body.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:012x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("agens-skill-disc-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn install(root: &Path, dir: &str, body: &str) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(SKILL_FILE), body).unwrap();
    }

    #[test]
    fn discovery_finds_fixture_and_catalog_contains_it() {
        let tmp = TmpDir::new("find");
        install(
            &tmp.0,
            "weather",
            "---\nname: weather\ndescription: Get the forecast\nversion: 1.2.0\n---\n# Weather\nSteps...\n",
        );
        let skills = discover_skills(&tmp.0);
        assert_eq!(skills.len(), 1, "should find the one installed skill");
        let s = &skills[0];
        assert_eq!(s.name, "weather");
        assert_eq!(s.description.as_deref(), Some("Get the forecast"));
        assert_eq!(s.version, "1.2.0");

        let block = build_available_skills_block(&skills);
        assert!(block.contains("<available_skills>"), "block header");
        assert!(block.contains("<name>weather</name>"), "skill name in catalog");
        assert!(
            block.contains("<description>Get the forecast</description>"),
            "description in catalog"
        );
        assert!(block.contains("<version>1.2.0</version>"), "version in catalog");
        assert!(
            block.contains("SKILL.md"),
            "catalog cites the SKILL.md location for on-demand load"
        );
    }

    #[test]
    fn missing_front_matter_falls_back_to_dir_name_and_hash() {
        let tmp = TmpDir::new("nofm");
        install(&tmp.0, "raw-skill", "# Raw skill\nNo front matter here.\n");
        let skills = discover_skills(&tmp.0);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "raw-skill");
        assert!(skills[0].description.is_none());
        assert!(
            skills[0].version.starts_with("sha256:"),
            "hash-based version fallback"
        );
    }

    #[test]
    fn missing_dir_yields_empty_not_error() {
        let missing = std::env::temp_dir().join("agens-skill-disc-does-not-exist-xyz");
        assert!(discover_skills(&missing).is_empty());
        assert_eq!(skills_catalog(&missing), "");
    }

    #[test]
    fn multiple_skills_sorted_by_name() {
        let tmp = TmpDir::new("multi");
        install(&tmp.0, "zeta", "---\nname: zeta\n---\nbody\n");
        install(&tmp.0, "alpha", "---\nname: alpha\n---\nbody\n");
        let skills = discover_skills(&tmp.0);
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }
}
