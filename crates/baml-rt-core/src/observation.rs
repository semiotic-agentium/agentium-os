//! Operator observation invalidation — broadcast after provenance commits.

/// Bit flags indicating which operator read surfaces may have changed.
pub mod kinds {
    pub const TRANSCRIPT: u8 = 1;
    pub const PLANNING: u8 = 2;
    pub const OPS: u8 = 4;
    pub const ALL: u8 = TRANSCRIPT | PLANNING | OPS;
}

/// Broadcast after a successful context-scoped provenance commit (and on
/// selected A2A task updates). Drives `/contexts/*/observe/stream` and
/// conversation-history SSE subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationUpdate {
    pub context_id: String,
    pub task_id: Option<String>,
    pub kinds: u8,
}

impl ObservationUpdate {
    #[must_use]
    pub fn affects_transcript(&self) -> bool {
        self.kinds & kinds::TRANSCRIPT != 0
    }

    #[must_use]
    pub fn affects_planning(&self) -> bool {
        self.kinds & kinds::PLANNING != 0
    }

    #[must_use]
    pub fn affects_ops(&self) -> bool {
        self.kinds & kinds::OPS != 0
    }
}
