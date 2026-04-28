//! Inlined skill category types (formerly from `pares-trainer::skill_detection`).

use serde::{Deserialize, Serialize};

/// A category of skill identified in a training cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SkillCategory {
    /// Coding examples — inner value is the dominant language (e.g. `"rust"`).
    Coding(String),
    /// Writing examples — inner value is the dominant genre (e.g. `"essay"`).
    Writing(String),
    /// Analysis examples — inner value is the dominant domain (e.g. `"financial"`).
    Analysis(String),
    /// Domain-specific examples that don't fit the other categories.
    DomainSpecific(String),
}
