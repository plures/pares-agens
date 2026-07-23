//! `apply_patch` — multi-file patch application in the OpenClaw `*** Begin Patch`
//! envelope, for tool-surface parity with OpenClaw's `apply_patch`.
//!
//! Format (one patch may touch many files):
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.txt
//! +line one
//! +line two
//! *** Update File: path/existing.txt
//! @@ context
//! -removed line
//! +added line
//!  unchanged line
//! *** Delete File: path/gone.txt
//! *** End Patch
//! ```
//!
//! Update hunks are matched against the current file content: a `-`/` ` (removed
//! or context) run must occur contiguously in the file; it is replaced by the
//! `+`/` ` (added or context) run. Multiple `@@` hunks per file are applied in
//! order, each to the region following the previous one.
//!
//! **Atomicity:** the whole patch is parsed and every hunk is validated (files
//! exist / don't already exist / context matches) BEFORE any write. If any
//! operation would fail, nothing is written and an error naming the failing file
//! is returned. This mirrors OpenClaw's all-or-nothing patch semantics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A single file operation parsed from a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Create a new file with exactly these lines (each was a `+` line).
    Add {
        /// Patch-relative path of the file to create.
        path: String,
        /// Full contents to write.
        contents: String,
    },
    /// Delete an existing file.
    Delete {
        /// Patch-relative path of the file to delete.
        path: String,
    },
    /// Apply one or more hunks to an existing file.
    Update {
        /// Patch-relative path of the file to update.
        path: String,
        /// Ordered hunks to apply.
        hunks: Vec<Hunk>,
    },
}

/// One `@@` hunk: a contiguous run to find (removed + context) and the run to
/// replace it with (added + context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Lines that must be found contiguously (context + removed), in order.
    pub find: Vec<String>,
    /// Replacement lines (context + added), in order.
    pub replace: Vec<String>,
}

/// Error from parsing a patch envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "patch parse error: {}", self.0)
    }
}

/// Parse the full `*** Begin Patch` … `*** End Patch` envelope into ordered ops.
pub fn parse_patch(input: &str) -> Result<Vec<FileOp>, ParseError> {
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;

    // Skip to the Begin marker (tolerate leading blank lines / stray text).
    while i < lines.len() && lines[i].trim() != "*** Begin Patch" {
        i += 1;
    }
    if i >= lines.len() {
        return Err(ParseError("missing '*** Begin Patch' header".into()));
    }
    i += 1; // consume Begin

    let mut ops: Vec<FileOp> = Vec::new();
    let mut saw_end = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end();

        if trimmed.trim() == "*** End Patch" {
            saw_end = true;
            break;
        } else if let Some(path) = trimmed.strip_prefix("*** Add File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ParseError("'*** Add File:' with empty path".into()));
            }
            i += 1;
            let mut body_lines: Vec<&str> = Vec::new();
            while i < lines.len() && !is_section_marker(lines[i]) {
                let l = lines[i];
                // Added-file bodies use `+` prefixes; a bare line is tolerated.
                body_lines.push(l.strip_prefix('+').unwrap_or(l));
                i += 1;
            }
            let mut body = body_lines.join("\n");
            // A file written with a trailing newline is the norm.
            if !body.is_empty() {
                body.push('\n');
            }
            ops.push(FileOp::Add {
                path,
                contents: body,
            });
        } else if let Some(path) = trimmed.strip_prefix("*** Delete File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ParseError("'*** Delete File:' with empty path".into()));
            }
            ops.push(FileOp::Delete { path });
            i += 1;
        } else if let Some(path) = trimmed.strip_prefix("*** Update File: ") {
            let path = path.trim().to_string();
            if path.is_empty() {
                return Err(ParseError("'*** Update File:' with empty path".into()));
            }
            i += 1;
            let mut hunks: Vec<Hunk> = Vec::new();
            // Each hunk starts at an `@@` line; lines until the next `@@` or
            // section marker are the hunk body.
            while i < lines.len() && !is_section_marker(lines[i]) {
                if lines[i].starts_with("@@") {
                    i += 1; // consume the @@ line (its trailing text is context hint only)
                }
                let mut find = Vec::new();
                let mut replace = Vec::new();
                let mut any = false;
                while i < lines.len()
                    && !is_section_marker(lines[i])
                    && !lines[i].starts_with("@@")
                {
                    let l = lines[i];
                    if let Some(rest) = l.strip_prefix('+') {
                        replace.push(rest.to_string());
                    } else if let Some(rest) = l.strip_prefix('-') {
                        find.push(rest.to_string());
                    } else {
                        // Context line (leading space, or bare). Preserve the
                        // exact text minus a single leading space if present.
                        let ctx = l.strip_prefix(' ').unwrap_or(l).to_string();
                        find.push(ctx.clone());
                        replace.push(ctx);
                    }
                    any = true;
                    i += 1;
                }
                if any {
                    hunks.push(Hunk { find, replace });
                }
            }
            if hunks.is_empty() {
                return Err(ParseError(format!(
                    "'*** Update File: {path}' has no hunks"
                )));
            }
            ops.push(FileOp::Update { path, hunks });
        } else if trimmed.is_empty() {
            i += 1; // tolerate blank lines between sections
        } else {
            return Err(ParseError(format!(
                "unexpected line inside patch: {trimmed:?}"
            )));
        }
    }

    if !saw_end {
        return Err(ParseError("missing '*** End Patch' trailer".into()));
    }
    if ops.is_empty() {
        return Err(ParseError("patch contains no file operations".into()));
    }
    Ok(ops)
}

fn is_section_marker(line: &str) -> bool {
    let t = line.trim_end();
    t == "*** End Patch"
        || t.starts_with("*** Add File: ")
        || t.starts_with("*** Update File: ")
        || t.starts_with("*** Delete File: ")
}

/// Apply an update's hunks to file `content`, returning the new content, or an
/// error if any hunk's `find` run is not located (searching forward from the
/// previous hunk's end so ordered hunks compose correctly).
pub fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let orig: Vec<&str> = content.lines().collect();
    let had_trailing_newline = content.ends_with('\n');
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize; // index into orig not yet emitted

    for (hi, hunk) in hunks.iter().enumerate() {
        if hunk.find.is_empty() {
            // Pure insertion (all `+`): append at the cursor.
            out.extend(hunk.replace.iter().cloned());
            continue;
        }
        // Find `hunk.find` as a contiguous slice at or after `cursor`.
        let start = find_contiguous(&orig, &hunk.find, cursor).ok_or_else(|| {
            format!(
                "hunk #{} did not match (could not locate the -/context run)",
                hi + 1
            )
        })?;
        // Emit unchanged lines between cursor and the match.
        for l in &orig[cursor..start] {
            out.push((*l).to_string());
        }
        // Emit the replacement in place of the matched run.
        out.extend(hunk.replace.iter().cloned());
        cursor = start + hunk.find.len();
    }
    // Emit the remaining tail.
    for l in &orig[cursor..] {
        out.push((*l).to_string());
    }

    let mut result = out.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

/// Locate `needle` as a contiguous run of lines within `haystack` at index
/// `>= from`. Returns the start index of the match.
fn find_contiguous(haystack: &[&str], needle: &[String], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    (from..=last_start).find(|&start| {
        haystack[start..start + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(h, n)| *h == n.as_str())
    })
}

/// The outcome of applying a patch: the exact bytes to write per path, and the
/// set of paths to delete. Computed WITHOUT touching disk so the caller can
/// validate-then-commit atomically.
#[derive(Debug, Default)]
pub struct PatchPlan {
    /// path -> new file contents (Add + Update results).
    pub writes: BTreeMap<PathBuf, String>,
    /// paths to remove (Delete).
    pub deletes: Vec<PathBuf>,
    /// human-readable summary lines.
    pub summary: Vec<String>,
}

/// Build a [`PatchPlan`] by resolving each op against the current filesystem
/// state via the provided readers, WITHOUT mutating anything. Returns the first
/// validation error encountered (naming the file), so the caller writes nothing
/// on failure.
///
/// `resolve` maps a patch-relative path to an absolute path; `read` returns the
/// current contents of an existing file (or `None` if absent); `exists` reports
/// whether a path currently exists. Kept as closures so the module has zero I/O
/// dependency and is fully unit-testable.
pub fn plan_patch(
    ops: &[FileOp],
    resolve: impl Fn(&str) -> PathBuf,
    read: impl Fn(&Path) -> Option<String>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PatchPlan, String> {
    let mut plan = PatchPlan::default();
    for op in ops {
        match op {
            FileOp::Add { path, contents } => {
                let abs = resolve(path);
                if exists(&abs) {
                    return Err(format!("Add File: {path} already exists"));
                }
                plan.summary.push(format!("add {path}"));
                plan.writes.insert(abs, contents.clone());
            }
            FileOp::Delete { path } => {
                let abs = resolve(path);
                if !exists(&abs) {
                    return Err(format!("Delete File: {path} does not exist"));
                }
                plan.summary.push(format!("delete {path}"));
                plan.deletes.push(abs);
            }
            FileOp::Update { path, hunks } => {
                let abs = resolve(path);
                let current = read(&abs)
                    .ok_or_else(|| format!("Update File: {path} does not exist"))?;
                let updated = apply_hunks(&current, hunks)
                    .map_err(|e| format!("Update File: {path}: {e}"))?;
                plan.summary
                    .push(format!("update {path} ({} hunk(s))", hunks.len()));
                plan.writes.insert(abs, updated);
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_update_delete() {
        let patch = "\
*** Begin Patch
*** Add File: a.txt
+hello
+world
*** Update File: b.txt
@@
 keep
-old
+new
*** Delete File: c.txt
*** End Patch
";
        let ops = parse_patch(patch).expect("parse");
        assert_eq!(ops.len(), 3);
        match &ops[0] {
            FileOp::Add { path, contents } => {
                assert_eq!(path, "a.txt");
                assert_eq!(contents, "hello\nworld\n");
            }
            other => panic!("expected Add, got {other:?}"),
        }
        match &ops[1] {
            FileOp::Update { path, hunks } => {
                assert_eq!(path, "b.txt");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].find, vec!["keep", "old"]);
                assert_eq!(hunks[0].replace, vec!["keep", "new"]);
            }
            other => panic!("expected Update, got {other:?}"),
        }
        assert_eq!(ops[2], FileOp::Delete { path: "c.txt".into() });
    }

    #[test]
    fn apply_hunks_replaces_matched_run() {
        let content = "line1\nline2\nline3\n";
        let hunks = vec![Hunk {
            find: vec!["line2".into()],
            replace: vec!["line2-edited".into()],
        }];
        let out = apply_hunks(content, &hunks).expect("apply");
        assert_eq!(out, "line1\nline2-edited\nline3\n");
    }

    #[test]
    fn apply_hunks_multiple_ordered() {
        let content = "a\nb\nc\nd\n";
        let hunks = vec![
            Hunk {
                find: vec!["a".into()],
                replace: vec!["A".into()],
            },
            Hunk {
                find: vec!["c".into()],
                replace: vec!["C".into()],
            },
        ];
        let out = apply_hunks(content, &hunks).expect("apply");
        assert_eq!(out, "A\nb\nC\nd\n");
    }

    #[test]
    fn apply_hunks_errors_when_context_missing() {
        let content = "a\nb\n";
        let hunks = vec![Hunk {
            find: vec!["nonexistent".into()],
            replace: vec!["x".into()],
        }];
        assert!(apply_hunks(content, &hunks).is_err());
    }

    #[test]
    fn plan_is_atomic_add_conflict_aborts() {
        let ops = vec![
            FileOp::Add {
                path: "new.txt".into(),
                contents: "x\n".into(),
            },
            FileOp::Add {
                path: "exists.txt".into(),
                contents: "y\n".into(),
            },
        ];
        let resolve = |p: &str| PathBuf::from(p);
        let read = |_: &Path| None;
        let exists = |p: &Path| p == Path::new("exists.txt");
        let err = plan_patch(&ops, resolve, read, exists).unwrap_err();
        assert!(err.contains("exists.txt"), "got: {err}");
    }

    #[test]
    fn plan_update_and_add_succeeds() {
        let ops = vec![
            FileOp::Update {
                path: "f.txt".into(),
                hunks: vec![Hunk {
                    find: vec!["old".into()],
                    replace: vec!["new".into()],
                }],
            },
            FileOp::Add {
                path: "g.txt".into(),
                contents: "created\n".into(),
            },
        ];
        let resolve = |p: &str| PathBuf::from(p);
        let read = |p: &Path| {
            if p == Path::new("f.txt") {
                Some("old\n".to_string())
            } else {
                None
            }
        };
        let exists = |p: &Path| p == Path::new("f.txt");
        let plan = plan_patch(&ops, resolve, read, exists).expect("plan");
        assert_eq!(plan.writes.get(Path::new("f.txt")).unwrap(), "new\n");
        assert_eq!(plan.writes.get(Path::new("g.txt")).unwrap(), "created\n");
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn missing_end_marker_errors() {
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+x\n";
        assert!(parse_patch(patch).is_err());
    }
}
