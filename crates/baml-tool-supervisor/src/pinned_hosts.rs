// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Pinned name resolution for granted egress hosts.
//!
//! The host resolves every declared tool host to exact addresses at grant time
//! and the VM network policy allows TCP only to those addresses. Hosts an
//! egress secret is bound to are the exception: they are left out of this
//! map and resolved through the VM's DNS forwarder, whose answer is the TLS
//! identity the network layer checks before substituting the secret. The
//! forwarder answers no other name. The host hands the pinned mapping to the
//! supervisor in
//! [`PINNED_HOSTS_ENV`] (`ip host;ip host;...`), and the supervisor appends it
//! to `/etc/hosts` before serving, so `getaddrinfo` (nsswitch `files`) answers
//! with exactly the allowed addresses.
//!
//! This must run in the guest, after boot: microsandbox's guest init rewrites
//! `/etc/hosts` (hostname, `host.microsandbox.internal`), so a rootfs patch
//! applied before boot does not survive.

use std::{io, net::IpAddr, path::Path};

/// Environment variable carrying the host-pinned `ip host` pairs.
pub const PINNED_HOSTS_ENV: &str = "KARAKARA_PINNED_HOSTS";

const BEGIN: &str = "# BEGIN karakara pinned tool hosts";
const END: &str = "# END karakara pinned tool hosts";

/// Parse `ip host;ip host`. Every entry is validated so the value cannot
/// inject arbitrary lines into `/etc/hosts`.
pub fn parse(value: &str) -> Result<Vec<(IpAddr, String)>, String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (ip, host) = entry
                .split_once(' ')
                .ok_or_else(|| format!("pinned host entry '{entry}' is not 'ip host'"))?;
            let ip: IpAddr = ip
                .parse()
                .map_err(|_| format!("pinned host entry '{entry}' has an invalid address"))?;
            let valid_host = !host.is_empty()
                && host.len() <= 253
                && host.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'
                })
                && host
                    .split('.')
                    .all(|label| !label.is_empty() && label.len() <= 63);
            if !valid_host {
                return Err(format!("pinned host entry '{entry}' has an invalid name"));
            }
            Ok((ip, host.to_string()))
        })
        .collect()
}

/// Replace the karakara block in `existing` with `entries` (idempotent).
pub fn render(existing: &str, entries: &[(IpAddr, String)]) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in existing.lines() {
        match line.trim() {
            BEGIN => skipping = true,
            END => skipping = false,
            _ if !skipping => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    if !entries.is_empty() {
        out.push_str(BEGIN);
        out.push('\n');
        for (ip, host) in entries {
            out.push_str(&format!("{ip}\t{host}\n"));
        }
        out.push_str(END);
        out.push('\n');
    }
    out
}

/// Apply [`PINNED_HOSTS_ENV`] to `hosts_file`. No-op when the variable is unset.
pub fn apply_from_env(hosts_file: &Path) -> Result<usize, String> {
    let Ok(value) = std::env::var(PINNED_HOSTS_ENV) else {
        return Ok(0);
    };
    let entries = parse(&value)?;
    let existing = match std::fs::read_to_string(hosts_file) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read {}: {error}", hosts_file.display())),
    };
    std::fs::write(hosts_file, render(&existing, &entries))
        .map_err(|error| format!("write {}: {error}", hosts_file.display()))?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_entries() {
        let entries =
            parse("203.0.113.7 api.open-meteo.com; 2001:db8::1 api.open-meteo.com").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "api.open-meteo.com");
        for bad in [
            "203.0.113.7",
            "nope api.example.com",
            "203.0.113.7 evil.com\n1.2.3.4 bank.com",
            "203.0.113.7 UPPER.example.com",
            "203.0.113.7 a..b",
            "203.0.113.7 two words",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn render_appends_block_and_is_idempotent() {
        let base = "127.0.0.1\tlocalhost vm\n172.16.0.5\thost.microsandbox.internal\n";
        let entries = parse("203.0.113.7 api.open-meteo.com").unwrap();
        let once = render(base, &entries);
        assert_eq!(
            once,
            format!("{base}{BEGIN}\n203.0.113.7\tapi.open-meteo.com\n{END}\n")
        );
        assert_eq!(render(&once, &entries), once);
        assert_eq!(render(&once, &[]), base);
    }

    #[test]
    fn apply_writes_the_file() {
        let dir = std::env::temp_dir().join(format!("pinned-hosts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hosts");
        std::fs::write(&file, "127.0.0.1\tlocalhost\n").unwrap();
        let entries = parse("198.51.100.2 geocoding-api.open-meteo.com").unwrap();
        let existing = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, render(&existing, &entries)).unwrap();
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("198.51.100.2\tgeocoding-api.open-meteo.com\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
