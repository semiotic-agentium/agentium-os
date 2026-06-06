// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! On-disk cache layout for approved external-tool snapshots.
//!
//! ```text
//! <root>/external-tools/
//!   tools/<tool_slug>/tool-snapshot.json
//!   pending/<tool_slug>/<snapshot_digest>.json
//! ```

use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use baml_rt_core::{BamlRtError, Result};

use crate::{ToolName, external_tools::ExternalToolSnapshot};

const EXTERNAL_TOOLS_DIR: &str = "external-tools";
const TOOLS_DIR: &str = "tools";
const PENDING_DIR: &str = "pending";
const SNAPSHOT_FILE: &str = "tool-snapshot.json";

pub fn external_tools_dir(root: &Path) -> PathBuf {
    root.join(EXTERNAL_TOOLS_DIR)
}

pub fn tool_slug(tool_name: &str) -> Result<String> {
    let parsed = ToolName::parse(tool_name)?;
    let bundle = parsed.bundle().as_str();
    let local = parsed.local().as_str();
    Ok(format!("{bundle}__{local}"))
}

pub fn approved_tool_dir(root: &Path, tool_name: &str) -> Result<PathBuf> {
    Ok(external_tools_dir(root)
        .join(TOOLS_DIR)
        .join(tool_slug(tool_name)?))
}

pub fn approved_snapshot_path(root: &Path, tool_name: &str) -> Result<PathBuf> {
    Ok(approved_tool_dir(root, tool_name)?.join(SNAPSHOT_FILE))
}

pub fn pending_tool_dir(root: &Path, tool_name: &str) -> Result<PathBuf> {
    Ok(external_tools_dir(root)
        .join(PENDING_DIR)
        .join(tool_slug(tool_name)?))
}

pub fn pending_snapshot_path(root: &Path, snapshot: &ExternalToolSnapshot) -> Result<PathBuf> {
    Ok(pending_tool_dir(root, &snapshot.tool.name)?
        .join(format!("{}.json", snapshot.snapshot_digest)))
}

pub fn write_approved_snapshot(root: &Path, snapshot: &ExternalToolSnapshot) -> io::Result<()> {
    let path = approved_snapshot_path(root, &snapshot.tool.name).map_err(invalid_data)?;
    write_json_atomic(&path, snapshot)
}

pub fn write_pending_snapshot(root: &Path, snapshot: &ExternalToolSnapshot) -> io::Result<()> {
    let path = pending_snapshot_path(root, snapshot).map_err(invalid_data)?;
    write_json_atomic(&path, snapshot)
}

/// Read snapshots from approved-slot cache dirs. Filters out stale/rejected/pending
/// records defensively in case an old cache or manual edit left them there.
pub fn read_approved_snapshots(root: &Path) -> Result<Vec<ExternalToolSnapshot>> {
    let tools_dir = external_tools_dir(root).join(TOOLS_DIR);
    let entries = match fs::read_dir(&tools_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_err(&tools_dir, err)),
    };

    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_err(&tools_dir, err))?;
        let path = entry.path().join(SNAPSHOT_FILE);
        if !path.is_file() {
            continue;
        }
        let snapshot = read_snapshot(&path)?;
        if snapshot.approval.state.is_approved() {
            snapshots.push(snapshot);
        }
    }
    snapshots.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
    Ok(snapshots)
}

pub fn read_pending_snapshots(root: &Path) -> Result<Vec<ExternalToolSnapshot>> {
    let pending_dir = external_tools_dir(root).join(PENDING_DIR);
    let entries = match fs::read_dir(&pending_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_err(&pending_dir, err)),
    };

    let mut snapshots = Vec::new();
    for tool_entry in entries {
        let tool_entry = tool_entry.map_err(|err| io_err(&pending_dir, err))?;
        let dir = tool_entry.path();
        if !dir.is_dir() {
            continue;
        }
        for snap_entry in fs::read_dir(&dir).map_err(|err| io_err(&dir, err))? {
            let snap_entry = snap_entry.map_err(|err| io_err(&dir, err))?;
            let path = snap_entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                snapshots.push(read_snapshot(&path)?);
            }
        }
    }
    snapshots.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
    Ok(snapshots)
}

pub fn read_snapshot(path: &Path) -> Result<ExternalToolSnapshot> {
    let raw = fs::read_to_string(path).map_err(|err| io_err(path, err))?;
    serde_json::from_str(&raw).map_err(|err| BamlRtError::InvalidArgumentWithSource {
        message: format!("failed to parse external tool snapshot {}", path.display()),
        source: Box::new(err),
    })
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

fn invalid_data(err: BamlRtError) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, err)
}

fn io_err(path: &Path, err: io::Error) -> BamlRtError {
    BamlRtError::InvalidArgumentWithSource {
        message: format!(
            "failed to access external tool cache entry {}",
            path.display()
        ),
        source: Box::new(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_slug_rejects_pathlike_names() {
        for bad in [
            "../../etc/cron.d/evil",
            "support/../../evil",
            "support/evil/extra",
            "support/.ssh",
            "support/wea ther",
            "support/weather.json",
            "support/weather\nx",
            "/support/weather",
            "support/",
        ] {
            assert!(tool_slug(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn tool_slug_uses_valid_tool_parts() {
        assert_eq!(
            tool_slug("support/weather-7").unwrap(),
            "support__weather-7"
        );
    }
}
