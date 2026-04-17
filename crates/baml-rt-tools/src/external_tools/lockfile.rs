use std::{collections::HashMap, fs, path::Path};

use baml_rt_core::{BamlRtError, Result};
use serde::{Deserialize, Serialize};

use super::metadata::{compute_tool_digest, read_external_metadata};
use crate::ToolName;

pub const EXTERNAL_TOOLS_LOCKFILE_NAME: &str = "external_tools.lock.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLockfileMode {
    Off,
    #[default]
    Permissive,
    Enforce,
}

impl ExternalLockfileMode {
    pub fn from_env() -> Self {
        match std::env::var("BAML_EXTERNAL_LOCKFILE_MODE") {
            Ok(value) => Self::parse(&value).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "permissive" => Some(Self::Permissive),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }

    pub fn should_enforce(self) -> bool {
        matches!(self, Self::Enforce)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolsLockfile {
    pub version: String,
    #[serde(default)]
    pub tools: Vec<ExternalToolLockEntry>,
}

impl ExternalToolsLockfile {
    pub fn empty() -> Self {
        Self {
            version: "1".to_string(),
            tools: Vec::new(),
        }
    }

    pub fn read_from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: format!("failed to read {}", path.display()),
            source: Box::new(e),
        })?;
        let parsed: Self =
            serde_json::from_str(&raw).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to parse {}", path.display()),
                source: Box::new(e),
            })?;
        if parsed.version != "1" {
            return Err(BamlRtError::InvalidArgument(format!(
                "unsupported external tools lockfile version '{}' (expected '1')",
                parsed.version
            )));
        }
        Ok(parsed)
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(BamlRtError::Json)?;
        fs::write(path, json).map_err(BamlRtError::Io)?;
        Ok(())
    }

    pub fn by_name(&self, name: &ToolName) -> Option<&ExternalToolLockEntry> {
        self.tools.iter().find(|entry| {
            let Some((bundle, local)) = entry.name.split_once('/') else {
                return false;
            };
            bundle == name.bundle().as_str() && local == name.local().as_str()
        })
    }

    pub fn from_tool_dirs(dirs: &[std::path::PathBuf]) -> Result<Self> {
        let mut entries = Vec::new();
        let mut seen = HashMap::<String, std::path::PathBuf>::new();

        for dir in dirs {
            let meta = read_external_metadata(dir)?;
            let digest = compute_tool_digest(dir)?;
            if let Some(prev) = seen.insert(meta.name.clone(), dir.clone()) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "duplicate external tool '{}' across lockfile sources: {} and {}",
                    meta.name,
                    prev.display(),
                    dir.display()
                )));
            }
            entries.push(ExternalToolLockEntry {
                name: meta.name,
                digest,
                abi_version: meta.tool_abi_version,
                protocol_version: "1".to_string(),
                oci_ref: None,
                platform: None,
                signer: None,
                capabilities: None,
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self {
            version: "1".to_string(),
            tools: entries,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolLockEntry {
    pub name: String,
    pub digest: String,
    pub abi_version: String,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
}
