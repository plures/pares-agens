//! Lightweight process diagnostics primitives.
//!
//! ADR-0010 single home for `VmRSS` / resident-set-size sampling. Previously
//! this logic was copy-pasted across the `cli`, `agens-plugin`, and `channels`
//! crates (and was tracked as `current_process_rss_kib` known-debt in the
//! duplication gate). It now lives here once; consumers import it.

/// Parse `VmRSS` (in kiB) out of `/proc/self/status`-style contents.
///
/// Returns the first `VmRSS:` value found, or `None` if absent/unparseable.
pub fn parse_vm_rss_kib(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with("VmRSS:") {
            return None;
        }
        line.split_whitespace().nth(1)?.parse::<u64>().ok()
    })
}

/// Current process resident-set size in kiB.
///
/// Linux-only (reads `/proc/self/status`); returns `None` on other targets.
pub fn current_process_rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_vm_rss_kib(&status)
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vm_rss_kib_extracts_numeric_value() {
        let status = "Name:\tpares-radix\nVmRSS:\t   42104 kB\nThreads:\t6\n";
        assert_eq!(parse_vm_rss_kib(status), Some(42104));
    }

    #[test]
    fn parse_vm_rss_kib_returns_none_when_absent() {
        let status = "Name:\tpares-radix\nThreads:\t6\n";
        assert_eq!(parse_vm_rss_kib(status), None);
    }
}
