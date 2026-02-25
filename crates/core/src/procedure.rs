use async_trait::async_trait;

use crate::event::Event;

/// A procedure is a named, async handler that reacts to an event.
#[async_trait]
pub trait Procedure: Send + Sync {
    /// Unique name for this procedure (e.g. `"on_message"`).
    fn name(&self) -> &str;

    /// The event kind this procedure handles (matches [`Event::kind`]).
    fn handles(&self) -> &str;

    /// Execute the procedure in response to the given event.
    async fn execute(&self, event: &Event) -> Vec<Event>;
}

/// Registry that maps event kinds to their registered procedures.
///
/// Procedures are loaded at startup from PluresDB state and stored here.
/// Multiple procedures may be registered for the same event kind.
#[derive(Default)]
pub struct ProcedureRegistry {
    procedures: Vec<Box<dyn Procedure>>,
}

impl ProcedureRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a procedure. Procedures are matched by [`Procedure::handles`].
    pub fn register(&mut self, procedure: Box<dyn Procedure>) {
        self.procedures.push(procedure);
    }

    /// Return all procedures that handle the given event kind.
    pub fn matching<'a>(&'a self, event_kind: &'a str) -> impl Iterator<Item = &'a dyn Procedure> {
        self.procedures
            .iter()
            .filter(move |p| p.handles() == event_kind)
            .map(|p| p.as_ref())
    }

    /// Number of registered procedures.
    pub fn len(&self) -> usize {
        self.procedures.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.procedures.is_empty()
    }
}
