//! `self_update` — re-export of the canonical self-update builders.
//!
//! Per ADR-0010, the self-update command/task logic lives in exactly one place:
//! [`pares_agens_agenda::self_update`]. This crate previously carried a verbatim
//! copy (Stage R2) which duplicated the fns AND their behavioral tests across the
//! `agenda` and `agens-plugin` crates — the textbook ADR-0010 violation.
//!
//! The host now DELEGATES: every path that used `crate::self_update::*`
//! (e.g. `crate::self_update::self_update_task_from_env()`,
//! `crate::self_update::build_self_update_task()`,
//! `crate::self_update::DEFAULT_SELF_UPDATE_INTERVAL_SECS`) resolves through this
//! re-export to the single source of truth. No logic lives here — only the
//! re-export. Behavioral tests live with the logic in `agenda`.
//!
//! NOTE: `pares_agens_agenda::self_update::build_self_update_task` /
//! `self_update_task_from_env` return `pares_agens_agenda::scheduler::Task`,
//! which is exactly the type the host already schedules — so delegation is
//! behavior-preserving.

pub use pares_agens_agenda::self_update::{
    build_self_update_task, build_update_command, resolve_agens_dir, self_update_task_from_env,
    DEFAULT_PARES_AGENS_DIR, DEFAULT_SELF_UPDATE_INTERVAL_SECS, PROJECTS_SUBDIR,
};
