//! Startup restore: collapse duplicate persisted deployments that share the same agent package name.

use std::collections::HashMap;

use baml_rt_core::{DeploymentContentHash, DeploymentRecord, DeploymentStatus};

/// Prefer restore candidates so at most one row per `agent_name` is applied.
///
/// Returns deployments to pass to [`baml_rt_core::DeploymentManager::deploy_by_hash`] (sorted by
/// `agent_name`, then `content_hash`) and content hashes whose rows should be deleted from the
/// state store as superseded.
pub(crate) fn dedupe_deployments_for_restore(
    records: Vec<DeploymentRecord>,
) -> (Vec<DeploymentRecord>, Vec<DeploymentContentHash>) {
    let mut groups: HashMap<String, Vec<DeploymentRecord>> = HashMap::new();
    for rec in records {
        groups.entry(rec.agent_name.clone()).or_default().push(rec);
    }

    let mut to_restore: Vec<DeploymentRecord> = Vec::new();
    let mut to_remove: Vec<DeploymentContentHash> = Vec::new();

    for (_agent_name, mut group) in groups {
        if group.len() == 1 {
            to_restore.push(group.pop().expect("single deployment group"));
            continue;
        }

        let mut winner_idx = 0usize;
        for i in 1..group.len() {
            if better_for_restore(&group[i], &group[winner_idx]) {
                winner_idx = i;
            }
        }
        let winner = group.swap_remove(winner_idx);
        for loser in group {
            to_remove.push(loser.content_hash);
        }
        to_restore.push(winner);
    }

    to_restore.sort_by(|a, b| {
        a.agent_name
            .cmp(&b.agent_name)
            .then_with(|| a.content_hash.as_str().cmp(b.content_hash.as_str()))
    });

    (to_restore, to_remove)
}

fn better_for_restore(a: &DeploymentRecord, b: &DeploymentRecord) -> bool {
    match (&a.status, &b.status) {
        (DeploymentStatus::Active, DeploymentStatus::Failed) => true,
        (DeploymentStatus::Failed, DeploymentStatus::Active) => false,
        _ => {
            if a.deployed_at != b.deployed_at {
                a.deployed_at > b.deployed_at
            } else {
                a.content_hash.as_str() > b.content_hash.as_str()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;

    fn rec(
        name: &str,
        hash_suffix: &str,
        status: DeploymentStatus,
        deployed_at: &str,
    ) -> DeploymentRecord {
        let pad = "0".repeat(64usize.saturating_sub(hash_suffix.len()));
        let hash_str = format!("{pad}{hash_suffix}");
        DeploymentRecord {
            content_hash: DeploymentContentHash::from_str(&hash_str).expect("test hash"),
            agent_name: name.to_string(),
            deployed_at: deployed_at.to_string(),
            status,
            last_error: None,
            last_attempt_at: None,
            failure_count: 0,
        }
    }

    #[test]
    fn dedupe_keeps_single_row_per_agent_name() {
        let (restore, remove) = dedupe_deployments_for_restore(vec![
            rec("clickup-agent", "aaa", DeploymentStatus::Active, "100"),
            rec("clickup-agent", "bbb", DeploymentStatus::Active, "200"),
            rec("notion-agent", "ccc", DeploymentStatus::Active, "50"),
        ]);
        assert_eq!(remove.len(), 1);
        assert_eq!(restore.len(), 2);
        assert!(
            restore
                .iter()
                .any(|r| r.agent_name == "clickup-agent" && r.deployed_at == "200"),
            "newer deployed_at should win among Active"
        );
        assert!(
            restore
                .iter()
                .any(|r| r.agent_name == "notion-agent" && r.content_hash.as_str().ends_with("ccc")),
        );
    }

    #[test]
    fn dedupe_prefers_active_over_failed() {
        let (restore, remove) = dedupe_deployments_for_restore(vec![
            rec("a", "111", DeploymentStatus::Failed, "999"),
            rec("a", "222", DeploymentStatus::Active, "1"),
        ]);
        assert_eq!(remove.len(), 1);
        assert_eq!(restore.len(), 1);
        assert_eq!(restore[0].status, DeploymentStatus::Active);
        assert!(restore[0].content_hash.as_str().ends_with("222"));
    }

    #[test]
    fn dedupe_stable_sort_by_agent_name() {
        let (restore, _) = dedupe_deployments_for_restore(vec![
            rec("zebra", "aa", DeploymentStatus::Active, "1"),
            rec("apple", "bb", DeploymentStatus::Active, "1"),
        ]);
        assert_eq!(restore[0].agent_name, "apple");
        assert_eq!(restore[1].agent_name, "zebra");
    }
}
