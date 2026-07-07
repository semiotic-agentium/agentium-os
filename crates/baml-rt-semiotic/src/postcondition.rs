// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Post-tool postcondition verification (shell assertions).

use std::{process::Stdio, time::Duration};

use serde::{Deserialize, Serialize};

use crate::schema::Postcondition;

const PER_CMD_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostconditionKind {
    Pass,
    AssertionFailed,
    EnvError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostconditionResult {
    pub desc: String,
    pub ok: bool,
    pub kind: PostconditionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostconditionRun {
    pub results: Vec<PostconditionResult>,
    pub passed: bool,
    pub assertion_failures: u32,
    pub env_errors: u32,
}

fn foreign_path_roots(cmd: &str) -> bool {
    cmd.contains("/etc/") || cmd.contains("/usr/") || cmd.contains("/var/") || cmd.contains("..")
}

pub fn run_postconditions(postconditions: &[Postcondition], cwd: Option<&str>) -> PostconditionRun {
    let mut results = Vec::new();
    let mut assertion_failures = 0u32;
    let mut env_errors = 0u32;

    for pc in postconditions {
        if pc.cmd.trim().is_empty() {
            continue;
        }
        let desc = if pc.desc.is_empty() {
            pc.cmd.clone()
        } else {
            pc.desc.clone()
        };
        let (ok, kind, detail) = run_one(&pc.cmd, cwd);
        if !ok {
            match kind {
                PostconditionKind::AssertionFailed => assertion_failures += 1,
                PostconditionKind::EnvError => env_errors += 1,
                PostconditionKind::Pass => {}
            }
        }
        results.push(PostconditionResult {
            desc,
            ok,
            kind,
            detail,
        });
    }

    PostconditionRun {
        passed: assertion_failures == 0 && env_errors == 0,
        results,
        assertion_failures,
        env_errors,
    }
}

fn run_one(cmd: &str, cwd: Option<&str>) -> (bool, PostconditionKind, Option<String>) {
    let mut child = match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(cwd.unwrap_or("."))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                PostconditionKind::EnvError,
                Some(format!("spawn failed: {e}")),
            );
        }
    };

    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut detail = String::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let _ = out.read_to_string(&mut detail);
                }
                if detail.is_empty()
                    && let Some(mut err) = child.stderr.take()
                {
                    use std::io::Read;
                    let _ = err.read_to_string(&mut detail);
                }
                let detail = detail.trim().chars().take(200).collect::<String>();
                let code = status.code().unwrap_or(-1);
                if status.success() {
                    return (true, PostconditionKind::Pass, None);
                }
                if code == 126 || code == 127 || foreign_path_roots(cmd) {
                    return (
                        false,
                        PostconditionKind::EnvError,
                        Some(if detail.is_empty() {
                            format!("exit {code}")
                        } else {
                            detail
                        }),
                    );
                }
                return (
                    false,
                    PostconditionKind::AssertionFailed,
                    Some(if detail.is_empty() {
                        format!("exit {code}")
                    } else {
                        detail
                    }),
                );
            }
            Ok(None) => {
                if started.elapsed() >= PER_CMD_TIMEOUT {
                    let _ = child.kill();
                    return (
                        false,
                        PostconditionKind::EnvError,
                        Some(format!("timed out after {}s", PER_CMD_TIMEOUT.as_secs())),
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return (
                    false,
                    PostconditionKind::EnvError,
                    Some(format!("wait failed: {e}")),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn true_assertion_passes() {
        let run = run_postconditions(
            &[Postcondition {
                cmd: "true".into(),
                desc: "always ok".into(),
            }],
            None,
        );
        assert!(run.passed);
        assert_eq!(run.assertion_failures, 0);
    }

    #[test]
    fn false_assertion_fails() {
        let run = run_postconditions(
            &[Postcondition {
                cmd: "false".into(),
                desc: "fail".into(),
            }],
            None,
        );
        assert!(!run.passed);
        assert_eq!(run.assertion_failures, 1);
    }
}
