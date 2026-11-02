// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Best-effort per-call resource accounting (P4.8).
//!
//! Reads `/proc` only. Never blocks the terminal `Result`: failures yield zeroed
//! fields.

use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub wall_ms: u64,
    pub cpu_ms: u64,
    pub peak_rss_kb: u64,
    pub worker_pid: u32,
}

impl Usage {
    pub fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "wall_ms": self.wall_ms,
            "cpu_ms": self.cpu_ms,
            "peak_rss_kb": self.peak_rss_kb,
            "worker_pid": self.worker_pid,
        })
    }
}

pub fn measure(pid: u32, wall: Duration) -> Usage {
    let (cpu_ms, peak_rss_kb) = process_tree_usage(pid).unwrap_or((0, 0));
    Usage {
        wall_ms: wall.as_millis().min(u128::from(u64::MAX)) as u64,
        cpu_ms,
        peak_rss_kb,
        worker_pid: pid,
    }
}

fn process_tree_usage(pid: u32) -> Option<(u64, u64)> {
    let mut cpu_ticks = 0_u64;
    let mut peak_kb = 0_u64;
    let mut queue = vec![pid];
    let mut seen = 0;
    while let Some(pid) = queue.pop() {
        seen += 1;
        if seen > 256 {
            break;
        }
        cpu_ticks = cpu_ticks.saturating_add(stat_cpu_ticks(pid).unwrap_or(0));
        peak_kb = peak_kb.saturating_add(status_peak_rss_kb(pid).unwrap_or(0));
        if let Ok(children) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            queue.extend(
                children
                    .split_whitespace()
                    .filter_map(|value| value.parse::<u32>().ok()),
            );
        }
    }
    // Linux USER_HZ is 100 on every supported guest image.
    Some((cpu_ticks.saturating_mul(10), peak_kb))
}

fn stat_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 (`comm`) may contain spaces and parentheses.
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // After `)` the next field is 3 (state); utime/stime are 14/15.
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime.saturating_add(stime))
}

fn status_peak_rss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmHWM:")?;
        value.split_whitespace().next()?.parse::<u64>().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_the_current_process() {
        let usage = measure(std::process::id(), Duration::from_millis(5));
        assert_eq!(usage.wall_ms, 5);
        assert!(usage.peak_rss_kb > 0, "VmHWM must be reported by Linux");
    }
}
