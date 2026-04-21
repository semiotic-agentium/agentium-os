use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use baml_rt_core::{BamlRtError, Result};
use sha2::{Digest, Sha256};

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(BamlRtError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).map_err(BamlRtError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn canonical_bind_digest(dir: &Path) -> Result<String> {
    let root = fs::canonicalize(dir).map_err(BamlRtError::Io)?;
    if !root.is_dir() {
        return Err(BamlRtError::InvalidArgument(format!(
            "bind path is not a directory: {}",
            root.display()
        )));
    }

    let mut entries = Vec::new();
    collect_entries(&root, &root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    hasher.update(b"baml-bind-rootfs-v1\0");
    for (rel, kind) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        match kind {
            EntryKind::Dir(mode) => {
                hasher.update([b'd']);
                hasher.update(mode.to_le_bytes());
            }
            EntryKind::File(mode, content_hash) => {
                hasher.update([b'f']);
                hasher.update(mode.to_le_bytes());
                hasher.update(content_hash.as_bytes());
            }
            EntryKind::Symlink(mode, target) => {
                hasher.update([b'l']);
                hasher.update(mode.to_le_bytes());
                hasher.update(target.as_os_str().as_encoded_bytes());
            }
        }
        hasher.update([0]);
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[derive(Debug)]
enum EntryKind {
    Dir(u32),
    File(u32, String),
    Symlink(u32, PathBuf),
}

fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<(String, EntryKind)>) -> Result<()> {
    for child in fs::read_dir(dir).map_err(BamlRtError::Io)? {
        let child = child.map_err(BamlRtError::Io)?;
        let path = child.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| BamlRtError::InvalidArgument(format!("failed to relativize path: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");

        let metadata = fs::symlink_metadata(&path).map_err(BamlRtError::Io)?;
        let mode = mode_bits(&metadata);
        if metadata.is_dir() {
            out.push((rel.clone(), EntryKind::Dir(mode)));
            collect_entries(root, &path, out)?;
        } else if metadata.is_file() {
            out.push((rel, EntryKind::File(mode, file_sha256(&path)?)));
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path).map_err(BamlRtError::Io)?;
            out.push((rel, EntryKind::Symlink(mode, target)));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode_bits(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_bits(_meta: &fs::Metadata) -> u32 {
    0
}
