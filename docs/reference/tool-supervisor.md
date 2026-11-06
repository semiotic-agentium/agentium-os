# Tool supervisor

`crates/baml-tool-supervisor` is the guest-side half of external tools v2. It
ships as the entrypoint of every tool runtime image and is the only process that
talks to the host over vsock. It holds no `ToolGrant`. Network policy and
egress-bound secrets are enforced by the host outside the VM.

The same binary runs `karak tool test` in `--dev` mode, where it serves the
identical wire over stdio and isolates workers with bwrap instead of raw
clone/setuid. There is one supervisor implementation, two isolation backends.

## Topology

```
host runner                         guest microVM
ToolHost / VmPool  ── vsock ──▶     supervisor (pid 1, root)
  channel.rs        protocol v2       ├─ verify + unpack .ktool to tmpfs
                                      └─ worker per tool digest (uid, ns,
                                         rlimits, seccomp)
                                            node → MCP stdio server
```

The supervisor is an MCP client of the worker. It maps `protocol_v2` invokes to
MCP `tools/call`, checks the live `tools/list` result against the bundled
descriptors, and maps MCP results and notifications back to host frames.
`@karakara/tool` is the TypeScript authoring SDK; plain MCP servers work too.

## Wire

Messages are the types in `crates/baml-sandbox-protocol/src/protocol_v2.rs`.
Frames are length-prefixed (`[u32 BE len][bytes]`). Untyped frames mean the
reader is a state machine: a `LoadTool` header is followed by exactly
`bundle_len` raw bytes, split into 64 KiB chunks by the host.

| Host → guest | Handling |
| --- | --- |
| `Hello` | Checks protocol 2, replies `HelloAck`, logs the grant class |
| `LoadTool` | Reads the bundle, re-verifies `tool_digest`, unpacks, replies `LoadAck` |
| `Invoke` | Runs the call, forwards `Event`s, replies one terminal `Result` |
| `Cancel` | SIGKILLs that call's worker process group |
| `Secret` | Stages a reveal value in a sealed memfd for one request id |

Any decode error, unexpected message, or short/overlong bundle retires the
connection. A failed `LoadTool` audit replies `LoadAck{ok:false}` and keeps the
connection alive.

## Bundle handling

`baml-ktool` is the shared crate. Host and guest parse the same manifest and
descriptor and compute the same digest from the same bytes. The supervisor
recomputes `compute_tool_digest_v2(bytes, runtime_image_digest)` and compares it
with the header before writing anything.

Unpacking rejects symlinks and hardlinks, absolute paths, `..`, duplicate
entries, and a running total over `max_unpacked_bytes`. Files are created with
`create_new` and sanitized modes. A failed unpack removes its partial directory.

## Worker isolation

`WorkerBackend` has two production implementations and one explicitly unsafe
fallback.

- `RootCloneBackend` runs inside the VM as root. It creates mount, PID, IPC,
  UTS and cgroup namespaces (plus a loopback-only net namespace for tools that
  declared no egress), assigns a distinct uid/gid per tool digest, sets
  rlimits, installs a pinned seccomp allowlist, and scrubs the environment.
  `unshare(CLONE_NEWPID)` only moves future children, so the spawned process is
  a small shim that reaps the real worker and mirrors its status. The supervisor
  reaps orphaned descendants when it is PID 1.
- `BwrapBackend` runs on a dev box without root. It uses bwrap user, PID,
  mount, IPC, UTS and cgroup namespaces. Cross-tool isolation is namespace
  based, not uid based, because all workers share the host uid.
- `DirectBackend` is opt-in with `--backend direct`. It has no namespaces and
  no uid separation. Use it only when bwrap is unavailable, never inside a VM.

The seccomp profile is compiled into the binary and pinned by
`supervisor_abi`. The allowlist covers the Node 22 / libuv / glibc syscall
surface, including optional probes such as io_uring. Anything outside that
surface kills the worker.

## Calls

One warm worker per tool digest serves many invokes. The SDK owns the worker's
MCP request loop internally; tool authors cannot access its server or transport.

- Deadline: the supervisor takes `min(host deadline, host cap)` and kills the
  worker process group when it expires. A killed warm worker is dropped.
- Output: serialized `Result.output` over `max_result_bytes` is rejected with
  `truncated: true`. Worker stderr is read continuously and capped at
  `default_output_cap`; a worker that floods its log is killed and the result is
  reported truncated.
- Cancel: `Cancel{request_id}` kills that call's group. The terminal result is a
  transient error. A cancel that arrives before the worker starts is honored
  before the call is sent.
- A malformed frame or worker exit fails only that worker. The vsock channel
  survives and the next call cold-starts a worker.

Each call may emit an `Event{kind:"usage"}` before the result with wall time,
CPU time, peak RSS from `/proc`, and the worker pid. Accounting is best effort
and never blocks the result.

## Reveal secrets

Egress-bound secrets never enter the guest. A reveal secret is the approved
exception:

1. The host sends `Secret{request_id, name, value}` before the invoke.
2. The supervisor writes the value into a sealed memfd
   (`F_SEAL_WRITE|SHRINK|GROW|SEAL`).
3. A per-call worker is spawned with the fd duplicated to a fixed number. Env
   `KARAKARA_REVEAL_FDS` maps names to fd numbers. The value is never in argv,
   env values, or the invoke payload.
4. `revealTransport()` in the SDK reads the fd on first `ctx.secrets[name]`
   access and zeroes its buffer.
5. The fd closes when the call's inflight state is dropped.

Reveal values are part of `grant_class` through keyed fingerprints, so rotating
a value cannot reuse a VM that staged the old one.

## DevBackend

`karak tool test` builds a `.ktool` from the project (compiled entrypoint plus
vendored `node_modules`), starts `baml-tool-supervisor --dev`, and drives one
invoke over stdio. Locate the binary with `KARAKARA_SUPERVISOR_BIN`, a sibling
of `karak`, or `PATH`. `just build` builds it alongside `karak`.

`KARAKARA_SUPERVISOR_BACKEND` selects the backend, `bwrap` by default.
`KARAKARA_WORKER_HARNESS` overrides the worker entry and is ignored outside
`--dev`. Use `--backend direct` only when bwrap is unavailable.

## Limits

| Setting | Default | Override |
| --- | --- | --- |
| vsock port | 5000 | `KARAKARA_VSOCK_PORT` / `--vsock-port` |
| bundle bytes | 64 MiB | `MAX_KTOOL_BUNDLE_BYTES` |
| unpacked bytes | 256 MiB | `MAX_KTOOL_UNPACKED_BYTES` |
| worker address space | 4 GiB virtual | `KARAKARA_MEMORY_BYTES` |
| per-call max | 120 s | `DEFAULT_MAX_CALL_MS` |
| stderr capture | 1 MiB | `SupervisorLimits` |
| result size | 4 MiB | `SupervisorLimits` |
| warm workers | 16 | `SupervisorLimits` |
| idle worker timeout | 300 s | `KARAKARA_WORKER_IDLE_MS` |
| worker uid/gid base | 100000 | `SupervisorLimits` |

V8 reserves several GiB of virtual address space, so `RLIMIT_AS` is set well
above the VM memory grant. The grant remains the resident-memory bound.

## Testing

The default lane runs the supervisor over a duplex with a real node worker under
bwrap. Tests skip when bwrap or node is missing.

```bash
cargo test -p baml-ktool -p baml-tool-supervisor
just tool-supervisor-fuzz-smoke
```

The live KVM lane needs the image from `just tool-runtime-image`:

```bash
KARAKARA_KVM_IMAGE=registry/tool-runtime@sha256:… \
  cargo test -p baml-rt-tools --features sandbox-e2e-kvm \
  --test external_tools_supervisor_kvm
```

Fuzz targets live in `crates/baml-tool-supervisor/fuzz`:

```bash
cargo +nightly fuzz run bundle_unpack
cargo +nightly fuzz run wire_state_machine
```

The runtime image contract and build steps are in
[`docker/tool-runtime/README.md`](../../docker/tool-runtime/README.md).
