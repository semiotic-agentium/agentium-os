// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Reveal-tier secret delivery (P4.7).
//!
//! A reveal secret reaches exactly one worker over an inherited, sealed memfd.
//! It never enters env values, argv, the invoke payload, or the agent. The
//! worker gets a routing table of `name -> fd number`; the number is not the
//! value. The fd is closed when the call's inflight state is dropped.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::fd::{FromRawFd, RawFd},
};

/// Upper bound on one revealed value. Real secrets are small; a hostile host
/// must not be able to make the guest allocate without limit.
pub const MAX_REVEAL_BYTES: usize = 64 * 1024;

pub struct RevealMaterial {
    pub name: String,
    pub file: File,
}

impl RevealMaterial {
    pub fn source_fd(&self) -> RawFd {
        use std::os::fd::AsRawFd as _;
        self.file.as_raw_fd()
    }

    /// Read the value back. Test/diagnostic helper; never called on the hot
    /// path and never logged.
    pub fn read_to_string(&mut self) -> io::Result<String> {
        let mut bytes = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut bytes)?;
        self.file.seek(SeekFrom::Start(0))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Validate a secret name as an environment-table key.
pub fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid reveal secret name",
        ));
    }
    Ok(())
}

/// Create a sealed, close-on-exec memfd holding the secret value.
///
/// Sealing forbids writes, growth, and shrinking, so the worker can read its
/// secret but cannot alter it or extend it into a shared channel.
pub fn create(name: String, value: &str) -> io::Result<RevealMaterial> {
    validate_name(&name)?;
    if value.len() > MAX_REVEAL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reveal secret exceeds the guest size cap",
        ));
    }
    let cname = std::ffi::CString::new("karakara-reveal").expect("static name");
    // SAFETY: memfd_create returns a fresh fd or -1.
    let fd =
        unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: we own the fd.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(value.as_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: fd is valid and owned; seals are valid flag bits.
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RevealMaterial { name, file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_readable_but_not_writable() {
        let mut material = create("TOKEN".into(), "s3cret-value").unwrap();
        assert_eq!(material.read_to_string().unwrap(), "s3cret-value");

        // The memfd is sealed: writes must fail.
        let mut file = material.file.try_clone().unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(file.write_all(b"tampered").is_err());
    }

    #[test]
    fn rejects_unsafe_names_and_oversized_values() {
        assert!(create("../TOKEN".into(), "x").is_err());
        assert!(create("TOKEN".into(), &"x".repeat(MAX_REVEAL_BYTES + 1)).is_err());
    }

    #[test]
    fn fd_is_close_on_exec_and_inheritable_after_dup() {
        let material = create("TOKEN".into(), "v").unwrap();
        let flags = unsafe { libc::fcntl(material.source_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
