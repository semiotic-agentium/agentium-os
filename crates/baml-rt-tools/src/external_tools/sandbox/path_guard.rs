use std::path::{Path, PathBuf};

use baml_rt_core::{BamlRtError, Result};

pub fn canonicalize_bind_path(raw: &Path, roots: &[PathBuf]) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(raw).map_err(|e| {
        BamlRtError::InvalidArgument(format!(
            "bind path does not resolve: {}: {e}",
            raw.display()
        ))
    })?;

    if !canonical.is_dir() {
        return Err(BamlRtError::InvalidArgument(format!(
            "bind path is not a directory: {}",
            canonical.display()
        )));
    }

    if roots.is_empty() {
        return Err(BamlRtError::InvalidArgument(
            "bind rootfs is disabled: sandbox.bind_roots is empty".to_string(),
        ));
    }

    if !roots.iter().any(|r| canonical.starts_with(r)) {
        return Err(BamlRtError::InvalidArgument(format!(
            "bind path escapes allowlist: {}",
            canonical.display()
        )));
    }

    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_temp_dir(prefix: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("baml-bind-{}-{}", prefix, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn allows_path_inside_root() {
        let base = mk_temp_dir("allow");
        let root = base.join("root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let out = canonicalize_bind_path(&nested, std::slice::from_ref(&root)).unwrap();
        assert_eq!(out, std::fs::canonicalize(&nested).unwrap());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_empty_allowlist() {
        let base = mk_temp_dir("deny");
        let err = canonicalize_bind_path(&base, &[]).unwrap_err();
        assert!(err.to_string().contains("disabled"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_non_directory() {
        let base = mk_temp_dir("file");
        let file = base.join("f");
        std::fs::write(&file, "x").unwrap();
        let err = canonicalize_bind_path(&file, std::slice::from_ref(&base)).unwrap_err();
        assert!(err.to_string().contains("not a directory"));
        let _ = std::fs::remove_dir_all(base);
    }
}
