//! Optional TOML startup config for the runner.
//!
//! Provides path lists that would otherwise come from env vars
//! (`BAML_EXTERNAL_TOOLS_DIR`, `BAML_SANDBOX_BIND_ROOTS`). The runner merges
//! file values with env values; env wins when both are set so existing
//! deployments keep working without changes.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const EXTERNAL_TOOLS_DIR_ENV: &str = "BAML_EXTERNAL_TOOLS_DIR";
pub const SANDBOX_BIND_ROOTS_ENV: &str = "BAML_SANDBOX_BIND_ROOTS";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub external_tools: ExternalToolsSection,
    pub sandbox: SandboxSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalToolsSection {
    pub dirs: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxSection {
    pub bind: SandboxBindSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxBindSection {
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    File,
    Env,
    None,
}

impl PathSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PathSource::File => "file",
            PathSource::Env => "env",
            PathSource::None => "none",
        }
    }
}

pub struct ResolvedPaths {
    pub external_tools_dirs: Vec<PathBuf>,
    pub external_tools_source: PathSource,
    pub sandbox_bind_roots: Vec<PathBuf>,
    pub sandbox_bind_source: PathSource,
}

impl FileConfig {
    /// Load and parse a runner.toml file. Relative paths inside the file are
    /// resolved against the parent directory of the config file.
    pub fn load(path: &Path) -> Result<FileConfig> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("read runner config {}", path.display()))?;
        let mut config: FileConfig = toml::from_str(&body)
            .with_context(|| format!("parse runner config {}", path.display()))?;

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for dir in config.external_tools.dirs.iter_mut() {
            *dir = resolve_relative(base, dir);
        }
        for root in config.sandbox.bind.roots.iter_mut() {
            *root = resolve_relative(base, root);
        }
        Ok(config)
    }
}

fn resolve_relative(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Merge file values with env vars. Env replaces file when set and non-empty.
pub fn resolve_paths(file: &FileConfig) -> ResolvedPaths {
    let (external_tools_dirs, external_tools_source) =
        merge_path_list(&file.external_tools.dirs, EXTERNAL_TOOLS_DIR_ENV);
    let (sandbox_bind_roots, sandbox_bind_source) =
        merge_path_list(&file.sandbox.bind.roots, SANDBOX_BIND_ROOTS_ENV);
    ResolvedPaths {
        external_tools_dirs,
        external_tools_source,
        sandbox_bind_roots,
        sandbox_bind_source,
    }
}

fn merge_path_list(file: &[PathBuf], env_key: &str) -> (Vec<PathBuf>, PathSource) {
    if let Ok(raw) = std::env::var(env_key) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let dirs: Vec<PathBuf> = trimmed
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            if !dirs.is_empty() {
                return (dirs, PathSource::Env);
            }
        }
    }
    if !file.is_empty() {
        return (file.to_vec(), PathSource::File);
    }
    (Vec::new(), PathSource::None)
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use tempfile::tempdir;
    use tokio::sync::Mutex;

    use super::*;

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn set_env(key: &str, value: &str) {
        // SAFETY: tests serialize env mutation via env_lock.
        unsafe { std::env::set_var(key, value) }
    }

    fn remove_env(key: &str) {
        // SAFETY: tests serialize env mutation via env_lock.
        unsafe { std::env::remove_var(key) }
    }

    #[test]
    fn parses_minimal_toml() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("runner.toml");
        fs::write(
            &path,
            r#"
[external_tools]
dirs = ["./tools/a", "/abs/tools/b"]

[sandbox.bind]
roots = ["./bind"]
"#,
        )
        .expect("write toml");

        let cfg = FileConfig::load(&path).expect("load");
        assert_eq!(cfg.external_tools.dirs.len(), 2);
        assert_eq!(cfg.external_tools.dirs[0], dir.path().join("tools/a"));
        assert_eq!(cfg.external_tools.dirs[1], PathBuf::from("/abs/tools/b"));
        assert_eq!(cfg.sandbox.bind.roots, vec![dir.path().join("bind")]);
    }

    #[test]
    fn empty_file_yields_empty_sections() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("runner.toml");
        fs::write(&path, "").expect("write empty toml");

        let cfg = FileConfig::load(&path).expect("load");
        assert!(cfg.external_tools.dirs.is_empty());
        assert!(cfg.sandbox.bind.roots.is_empty());
    }

    #[test]
    fn rejects_unknown_field() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("runner.toml");
        fs::write(
            &path,
            r#"
[external_tools]
dirs = ["./x"]
unknown = true
"#,
        )
        .expect("write toml");

        let err = FileConfig::load(&path).expect_err("unknown field must error");
        assert!(
            err.to_string().contains("unknown") || err.to_string().contains("unknown field"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn merge_env_replaces_file() {
        let _g = env_lock().lock().await;
        remove_env(EXTERNAL_TOOLS_DIR_ENV);
        remove_env(SANDBOX_BIND_ROOTS_ENV);

        let file = FileConfig {
            external_tools: ExternalToolsSection {
                dirs: vec![PathBuf::from("/from-file")],
            },
            sandbox: SandboxSection::default(),
        };
        set_env(EXTERNAL_TOOLS_DIR_ENV, "/from-env-a:/from-env-b");

        let resolved = resolve_paths(&file);
        assert_eq!(resolved.external_tools_source, PathSource::Env);
        assert_eq!(
            resolved.external_tools_dirs,
            vec![
                PathBuf::from("/from-env-a"),
                PathBuf::from("/from-env-b"),
            ]
        );

        remove_env(EXTERNAL_TOOLS_DIR_ENV);
    }

    #[tokio::test]
    async fn merge_falls_back_to_file_when_env_unset() {
        let _g = env_lock().lock().await;
        remove_env(EXTERNAL_TOOLS_DIR_ENV);
        remove_env(SANDBOX_BIND_ROOTS_ENV);

        let file = FileConfig {
            external_tools: ExternalToolsSection {
                dirs: vec![PathBuf::from("/from-file")],
            },
            sandbox: SandboxSection {
                bind: SandboxBindSection {
                    roots: vec![PathBuf::from("/bind-from-file")],
                },
            },
        };

        let resolved = resolve_paths(&file);
        assert_eq!(resolved.external_tools_source, PathSource::File);
        assert_eq!(resolved.external_tools_dirs, vec![PathBuf::from("/from-file")]);
        assert_eq!(resolved.sandbox_bind_source, PathSource::File);
        assert_eq!(
            resolved.sandbox_bind_roots,
            vec![PathBuf::from("/bind-from-file")]
        );
    }

    #[tokio::test]
    async fn merge_empty_when_neither_set() {
        let _g = env_lock().lock().await;
        remove_env(EXTERNAL_TOOLS_DIR_ENV);
        remove_env(SANDBOX_BIND_ROOTS_ENV);

        let resolved = resolve_paths(&FileConfig::default());
        assert!(resolved.external_tools_dirs.is_empty());
        assert_eq!(resolved.external_tools_source, PathSource::None);
        assert!(resolved.sandbox_bind_roots.is_empty());
        assert_eq!(resolved.sandbox_bind_source, PathSource::None);
    }

    #[tokio::test]
    async fn merge_treats_blank_env_as_unset() {
        let _g = env_lock().lock().await;
        remove_env(EXTERNAL_TOOLS_DIR_ENV);
        remove_env(SANDBOX_BIND_ROOTS_ENV);

        let file = FileConfig {
            external_tools: ExternalToolsSection {
                dirs: vec![PathBuf::from("/from-file")],
            },
            sandbox: SandboxSection::default(),
        };
        set_env(EXTERNAL_TOOLS_DIR_ENV, "   ");

        let resolved = resolve_paths(&file);
        assert_eq!(resolved.external_tools_source, PathSource::File);
        assert_eq!(resolved.external_tools_dirs, vec![PathBuf::from("/from-file")]);

        remove_env(EXTERNAL_TOOLS_DIR_ENV);
    }
}
