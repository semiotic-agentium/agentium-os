// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Runner identity that owns this runner's tool VMs (VM names and the
//! [`super::TOOL_VM_OWNER_LABEL`] owner label).

use baml_rt_core::{BamlRtError, Result};
use uuid::Uuid;

/// Fresh `runner_id` per process boot. Nothing persists it, so VMs a previous
/// boot left behind are never recognised as this runner's: stopped VMs keep
/// their rootfs until removed. Long-lived runners use
/// [`load_or_create_runner_id`] instead.
pub fn fresh_runner_id() -> String {
    Uuid::new_v4().to_string()
}

const RUNNER_ID_FILE: &str = "tool-runner-id";

/// Runner identity that survives restarts: read `<state_dir>/tool-runner-id`,
/// or create it with a fresh UUID. It owns this runner's tool VMs (VM names
/// and the owner label), so a restarted runner can reclaim what its previous
/// run left behind without touching VMs of another runner.
pub fn load_or_create_runner_id(state_dir: &std::path::Path) -> Result<String> {
    let path = state_dir.join(RUNNER_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let id = raw.trim();
            Uuid::parse_str(id).map_err(|_| {
                BamlRtError::InvalidArgument(format!(
                    "{} does not hold a runner UUID; remove it to mint a new identity",
                    path.display()
                ))
            })?;
            Ok(id.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(state_dir).map_err(|e| {
                BamlRtError::InvalidArgument(format!("create {}: {e}", state_dir.display()))
            })?;
            let id = fresh_runner_id();
            // Write-then-rename so a crash never leaves a truncated identity.
            let tmp = state_dir.join(format!("{RUNNER_ID_FILE}.tmp"));
            std::fs::write(&tmp, format!("{id}\n"))
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(|e| {
                    BamlRtError::InvalidArgument(format!("write {}: {e}", path.display()))
                })?;
            Ok(id)
        }
        Err(error) => Err(BamlRtError::InvalidArgument(format!(
            "read {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod runner_id_tests {
    use super::*;

    #[test]
    fn runner_id_is_created_once_and_survives_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let first = load_or_create_runner_id(&state).unwrap();
        assert!(Uuid::parse_str(&first).is_ok());
        assert_eq!(load_or_create_runner_id(&state).unwrap(), first);
        assert!(!state.join(format!("{RUNNER_ID_FILE}.tmp")).exists());
    }

    #[test]
    fn corrupt_runner_id_fails_instead_of_minting_a_new_owner() {
        // A silently re-minted id would orphan every VM the old id owns.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RUNNER_ID_FILE), "not-a-uuid\n").unwrap();
        assert!(load_or_create_runner_id(dir.path()).is_err());
    }
}
