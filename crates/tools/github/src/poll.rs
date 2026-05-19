//! GitHub Issues polling for the task-daemon substrate.
//!
//! List/issue polling is not implemented yet. When added, this module should own the
//! GitHub API integration; task-daemon will hold a thin `TaskSource` adapter and publish
//! via `batch_from_issue_records` in [`crate::source_records`].

use serde::{Deserialize, Serialize};

/// Persisted poll state for GitHub Issues (reserved for future polling).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubPollState {}
