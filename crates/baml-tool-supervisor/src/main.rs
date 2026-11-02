// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `baml-tool-supervisor` — guest init inside the runtime image, and the
//! DevBackend for `karak tool test`.
//!
//! Prod: dial the host over `AF_VSOCK` (CID 2) and serve protocol v2.
//! Dev (`--dev`): serve the same wire over stdin/stdout, with bwrap isolation.

use std::{path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use baml_sandbox_protocol::codec::FrameWriter;
use baml_tool_supervisor::{
    BwrapBackend, DirectBackend, Supervisor, SupervisorLimits, WorkerBackend, transport,
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Root,
    Bwrap,
    Direct,
}

struct Options {
    dev: bool,
    backend: Backend,
    root: PathBuf,
    vsock_port: u32,
    connect_timeout: Duration,
    /// `Trusted` tier (P5.1): dial the host over this `AF_UNIX` path instead of
    /// vsock/stdio. The process is unsandboxed (operator-trusted).
    uds: Option<PathBuf>,
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("baml-tool-supervisor: {message}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("baml-tool-supervisor: cannot start runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("baml-tool-supervisor: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(options: Options) -> Result<(), String> {
    let mut limits = SupervisorLimits::from_env();
    if options.vsock_port != baml_tool_supervisor::limits::DEFAULT_VSOCK_PORT {
        limits.vsock_port = options.vsock_port;
    }
    if !options.dev && options.uds.is_none() {
        // The harness override is a DevBackend/trusted-test knob only. In the VM
        // the image contract fixes the worker entry; the vsock path clears it.
        limits.worker_harness = None;
    }
    let backend: Arc<dyn WorkerBackend> = match options.backend {
        Backend::Root => {
            #[cfg(target_os = "linux")]
            {
                if !baml_tool_supervisor::RootCloneBackend::running_as_root() {
                    return Err(
                        "the root backend requires uid 0 (run `--dev` on a dev box)".to_string()
                    );
                }
                Arc::new(baml_tool_supervisor::RootCloneBackend)
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err("the root backend is Linux-only".to_string());
            }
        }
        Backend::Bwrap => {
            let bwrap = BwrapBackend::default();
            if !bwrap.available() {
                return Err(
                    "bwrap is not available; install bubblewrap or pass `--backend direct` \
                     (no isolation, dev only)"
                        .to_string(),
                );
            }
            Arc::new(bwrap)
        }
        Backend::Direct => {
            eprintln!(
                "baml-tool-supervisor: WARNING direct backend selected: no namespace or uid isolation"
            );
            Arc::new(DirectBackend)
        }
    };

    let (outbound, receiver) = mpsc::unbounded_channel();
    let (channel, mut writer) = if let Some(path) = &options.uds {
        let channel = transport::connect_uds(path, options.connect_timeout)
            .await
            .map_err(|error| format!("cannot dial host over uds: {error}"))?;
        transport::split(channel)
    } else if options.dev {
        transport::split(transport::stdio_channel())
    } else {
        let channel = transport::connect_vsock(options.vsock_port, options.connect_timeout)
            .await
            .map_err(|error| format!("cannot dial host over vsock: {error}"))?;
        transport::split(channel)
    };

    let writer_task = tokio::spawn(async move {
        write_loop(&mut writer, receiver).await;
    });

    // DevBackend and host-spawned trusted roots are per-run scratch space.
    let ephemeral_root = (options.dev || options.uds.is_some()).then(|| options.root.clone());
    let supervisor = Supervisor::new(limits, options.root, backend, outbound)
        .map_err(|error| format!("cannot initialize supervisor: {error}"))?;
    eprintln!(
        "baml-tool-supervisor: serving ({})",
        supervisor.backend_name()
    );
    supervisor.clone().spawn_idle_reaper();
    supervisor.spawn_orphan_reaper();
    let result = supervisor.clone().run(channel).await;
    supervisor.shutdown().await;
    writer_task.abort();
    if let Some(root) = ephemeral_root {
        let _ = std::fs::remove_dir_all(root);
    }
    result.map_err(|error| error.to_string())
}

async fn write_loop(
    writer: &mut FrameWriter,
    mut receiver: mpsc::UnboundedReceiver<baml_sandbox_protocol::protocol_v2::GuestMessage>,
) {
    while let Some(message) = receiver.recv().await {
        let value = match serde_json::to_value(&message) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("baml-tool-supervisor: cannot encode guest message: {error}");
                break;
            }
        };
        if let Err(error) = writer.send(&value).await {
            eprintln!("baml-tool-supervisor: channel write failed: {error}");
            break;
        }
    }
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut dev = false;
    let mut backend = None;
    let mut root = None;
    let mut vsock_port = baml_tool_supervisor::limits::DEFAULT_VSOCK_PORT;
    let mut connect_timeout = Duration::from_secs(15);
    let mut uds = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dev" => dev = true,
            "--backend" => {
                let value = args.next().ok_or("--backend needs a value")?;
                backend = Some(match value.as_str() {
                    "root" => Backend::Root,
                    "bwrap" => Backend::Bwrap,
                    "direct" => Backend::Direct,
                    other => return Err(format!("unknown backend '{other}'")),
                });
            }
            "--root" => root = Some(PathBuf::from(args.next().ok_or("--root needs a value")?)),
            "--uds" => uds = Some(PathBuf::from(args.next().ok_or("--uds needs a value")?)),
            "--vsock-port" => {
                vsock_port = args
                    .next()
                    .ok_or("--vsock-port needs a value")?
                    .parse()
                    .map_err(|_| "invalid --vsock-port".to_string())?;
                if vsock_port == 0 {
                    return Err("--vsock-port must be nonzero".to_string());
                }
            }
            "--connect-timeout-ms" => {
                let ms: u64 = args
                    .next()
                    .ok_or("--connect-timeout-ms needs a value")?
                    .parse()
                    .map_err(|_| "invalid --connect-timeout-ms".to_string())?;
                connect_timeout = Duration::from_millis(ms.max(1));
            }
            "--help" | "-h" => {
                println!(
                    "baml-tool-supervisor [--dev] [--backend root|bwrap|direct] \
                     [--root DIR] [--vsock-port N] [--uds PATH]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    let trusted = uds.is_some();
    if dev && trusted {
        return Err("--dev and --uds are mutually exclusive transports".to_string());
    }
    // Trusted (--uds) tools are unsandboxed host processes, so there is no VM
    // and the root backend is meaningless; default to the direct backend.
    let backend = backend.unwrap_or(if dev {
        Backend::Bwrap
    } else if trusted {
        Backend::Direct
    } else {
        Backend::Root
    });
    if dev && backend == Backend::Root {
        return Err("--dev cannot use the root backend (use bwrap or direct)".to_string());
    }
    if trusted {
        if backend == Backend::Root {
            return Err(
                "the trusted (--uds) transport runs unsandboxed; use `--backend direct`"
                    .to_string(),
            );
        }
    } else if !dev && backend != Backend::Root {
        return Err("a VM supervisor must use the root backend".to_string());
    }
    let root = root.unwrap_or_else(|| {
        if dev || trusted {
            std::env::temp_dir().join(format!(
                "karakara-supervisor-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ))
        } else {
            PathBuf::from("/run/ktool")
        }
    });
    Ok(Options {
        dev,
        backend,
        root,
        vsock_port,
        connect_timeout,
        uds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn dev_defaults_to_bwrap_and_a_private_root() {
        let options = parse(args(&["--dev"])).unwrap();
        assert!(options.dev);
        assert_eq!(options.backend, Backend::Bwrap);
        assert!(options.root.is_absolute());
    }

    #[test]
    fn prod_requires_the_root_backend() {
        assert!(parse(args(&["--backend", "bwrap"])).is_err());
        let options = parse(args(&[])).unwrap();
        assert_eq!(options.backend, Backend::Root);
        assert_eq!(options.root, PathBuf::from("/run/ktool"));
    }
}
