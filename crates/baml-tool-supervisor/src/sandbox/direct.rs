// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Explicitly unsafe fallback for developer machines without bwrap.
//!
//! It spawns the worker directly with rlimits and process-group semantics, but
//! no namespaces, no uid separation, and no filesystem isolation. Effective
//! boundary: the bundle directory. Never selectable in a VM: `--dev` is the
//! only entry point that allows it, and the supervisor logs a warning.

use std::{
    io,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

use tokio::process::Child;

use super::{WorkerBackend, WorkerCommand, WorkerSandbox, bwrap};

pub struct DirectBackend;

impl WorkerBackend for DirectBackend {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn spawn(&self, sandbox: &WorkerSandbox, command: WorkerCommand) -> io::Result<Child> {
        let mut env = command.env.clone();
        // No /tool mount: rewrite a test harness entry path to the host bundle.
        let entry = sandbox.bundle_dir.join(baml_ktool::KTOOL_NODE_ENTRYPOINT);
        if let Some(slot) = env
            .iter_mut()
            .find(|(name, _)| name == "KARAKARA_TOOL_ENTRY")
        {
            slot.1 = entry.to_string_lossy().into_owned();
        }
        let program = command
            .program
            .strip_prefix("/tool/")
            .map(|path| sandbox.bundle_dir.join(path).to_string_lossy().into_owned())
            .unwrap_or_else(|| command.program.clone());
        let args = command.args.iter().map(|arg| {
            arg.strip_prefix("/tool/")
                .map(|path| sandbox.bundle_dir.join(path).to_string_lossy().into_owned())
                .unwrap_or_else(|| arg.clone())
        });
        let mut std_command = Command::new(program);
        std_command
            .args(args)
            .current_dir(&command.cwd)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std_command.process_group(0);

        let limits = sandbox.rlimits;
        let reveals = command.reveal.clone();
        // SAFETY: async-signal-safe calls only.
        unsafe {
            std_command.pre_exec(move || {
                bwrap::apply_rlimits(&limits, false)?;
                bwrap::inherit_reveal_fds(&reveals)?;
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                Ok(())
            });
        }
        super::into_tokio(std_command).spawn()
    }
}
