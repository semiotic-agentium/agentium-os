//! Canonical labels for [`now_unix_ms`](crate::now_unix_ms) and
//! [`now_unix_secs`](crate::now_unix_secs).
//!
//! When `SystemTime::now()` falls before the UNIX epoch, both helpers emit a
//! `clock skew detected` warning carrying the supplied label on the
//! `clock_event` field. Using a constant from this module instead of a literal
//! makes typos a compile error and gives operators a single grep target for
//! the canonical set of clock-event sources.
//!
//! New call sites should add a constant here rather than passing an ad-hoc
//! string. Sibling read/write or per-source variants that are already
//! disambiguated by the surrounding span/trace should share one label
//! (e.g. `TASK_DAEMON_INTERPRETATION`, `TASK_DAEMON_BATCH`).
//!
//! Test-only labels are out of scope: passing a literal in a `#[cfg(test)]`
//! call is fine.

pub const A2A_STORE: &str = "a2a_store";
pub const A2A_TRANSPORT: &str = "a2a_transport";
pub const BUS_ENVELOPE: &str = "bus_envelope";
pub const CALLBACK_DISPATCH_CONTEXT: &str = "callback_dispatch_context";
pub const CONFIG_STORE: &str = "config_store";
pub const CONTEXT_ID_MINT: &str = "context_id_mint";
pub const CORRELATION_ID_MINT: &str = "correlation_id_mint";
pub const EPISODE_SNAPSHOT: &str = "episode_snapshot";
pub const EXTERNAL_QUARANTINE: &str = "external_quarantine";
pub const EXTERNAL_QUARANTINE_LIFT: &str = "external_quarantine_lift";
pub const GRAFANA_INGRESS: &str = "grafana_ingress";
pub const PROVENANCE_EVENT: &str = "provenance_event";
pub const REPOSITORY_TIMESTAMP: &str = "repository_timestamp";
pub const RUNNER_TIMESTAMP: &str = "runner_timestamp";
pub const SANDBOX_QUARANTINE: &str = "sandbox_quarantine";
pub const SLACK_INGRESS: &str = "slack_ingress";
pub const SLACK_LOOKBACK: &str = "slack_lookback";
pub const SLACK_POLL_NORMALIZE: &str = "slack_poll_normalize";
pub const SLACK_SOURCE_BOOTSTRAP: &str = "slack_source_bootstrap";
pub const SOCKET_MODE_ENQUEUE: &str = "socket_mode_enqueue";
pub const SOCKET_MODE_NORMALIZE: &str = "socket_mode_normalize";
pub const SYNTHETIC_MESSAGE: &str = "synthetic_message";
pub const SYNTHETIC_TASK: &str = "synthetic_task";
pub const SYSTEM_CALLBACK_CANCEL: &str = "system_callback_cancel";
pub const SYSTEM_CALLBACK_CHECKPOINT_RECONCILE: &str = "system_callback_checkpoint_reconcile";
pub const SYSTEM_CALLBACK_DUE_POLL: &str = "system_callback_due_poll";
pub const SYSTEM_CALLBACK_EMIT: &str = "system_callback_emit";
pub const SYSTEM_CALLBACK_MARK_EMITTED: &str = "system_callback_mark_emitted";
pub const SYSTEM_CALLBACK_SCHEDULE: &str = "system_callback_schedule";
pub const TASK_DAEMON_BATCH: &str = "task_daemon_batch";
pub const TASK_DAEMON_INTERPRETATION: &str = "task_daemon_interpretation";
pub const TASK_DAEMON_SEEN_TASK: &str = "task_daemon_seen_task";
