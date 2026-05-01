use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use crate::{
    claude::{
        ClaudeEngine, ClaudeEngineFactory, ClaudeEvent, MockClaudeEngineFactory, build_output,
    },
    types::{
        ERR_FAILED_PRECONDITION, ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_NOT_FOUND,
        ERR_UNAUTHENTICATED, SessionStatus, TOOL_NAME, err, ok,
    },
};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct PendingStep {
    step: &'static str,
    events: Vec<Value>,
    completion: Option<&'static str>,
    status: &'static str,
}

pub struct SessionState {
    hop: u64,
    status: SessionStatus,
    pending_steps: VecDeque<PendingStep>,
    last_activity: Instant,
    engine: Arc<dyn ClaudeEngine>,
    notify: Arc<Notify>,
}

impl SessionState {
    fn new(engine: Arc<dyn ClaudeEngine>) -> Self {
        Self {
            hop: 0,
            status: SessionStatus::Idle,
            pending_steps: VecDeque::new(),
            last_activity: Instant::now(),
            engine,
            notify: Arc::new(Notify::new()),
        }
    }
}

pub struct SessionStore {
    sessions: DashMap<String, Arc<Mutex<SessionState>>>,
    engine_factory: Arc<dyn ClaudeEngineFactory>,
    idle_ttl: Option<Duration>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            sessions: DashMap::new(),
            engine_factory: Arc::new(MockClaudeEngineFactory),
            idle_ttl: None,
        }
    }
}

impl SessionStore {
    pub fn with_engine_factory(mut self, engine_factory: Arc<dyn ClaudeEngineFactory>) -> Self {
        self.engine_factory = engine_factory;
        self
    }

    pub fn with_idle_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.idle_ttl = ttl;
        self
    }

    pub fn start_idle_reaper(self: Arc<Self>) {
        let Some(ttl) = self.idle_ttl else {
            return;
        };

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let now = Instant::now();
                let session_ids: Vec<String> = self
                    .sessions
                    .iter()
                    .map(|entry| entry.key().clone())
                    .collect();

                for session_id in session_ids {
                    let expired = if let Some(entry) = self.sessions.get(&session_id) {
                        let state = entry.value().lock().await;
                        now.saturating_duration_since(state.last_activity) > ttl
                    } else {
                        false
                    };

                    if expired {
                        if let Some((_, session)) = self.sessions.remove(&session_id) {
                            let engine = { Arc::clone(&session.lock().await.engine) };
                            let _ = engine.close().await;
                        }
                    }
                }
            }
        });
    }

    pub async fn open(&self, id: Value, params: Value) -> Value {
        let tool_name = params
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if tool_name != TOOL_NAME {
            return err(
                id,
                ERR_INVALID_PARAMS,
                format!("tool_name mismatch: expected {TOOL_NAME}, got {tool_name}"),
                "invalid_argument",
            );
        }
        let env_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if env_key.trim().is_empty() {
            return err(
                id,
                ERR_UNAUTHENTICATED,
                "missing or empty ANTHROPIC_API_KEY in sandbox environment",
                "unauthenticated",
            );
        }

        let session_id = format!("sess-{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed));
        let engine = match self.engine_factory.create(&session_id).await {
            Ok(engine) => engine,
            Err(message) => {
                return err(id, ERR_INTERNAL, message, "internal");
            }
        };

        self.sessions.insert(
            session_id.clone(),
            Arc::new(Mutex::new(SessionState::new(engine))),
        );
        ok(id, json!({ "session_id": session_id }))
    }

    pub async fn send(&self, id: Value, params: Value) -> Value {
        let Some(session_id) = params.get("session_id").and_then(Value::as_str) else {
            return err(
                id,
                ERR_INVALID_PARAMS,
                "missing session_id",
                "invalid_argument",
            );
        };

        let Some(entry) = self.sessions.get(session_id) else {
            return err(
                id,
                ERR_NOT_FOUND,
                format!("unknown session_id: {session_id}"),
                "not_found",
            );
        };

        let session = Arc::clone(entry.value());
        drop(entry);

        let (engine, notify) = {
            let mut state = session.lock().await;
            state.last_activity = Instant::now();
            if !matches!(state.status, SessionStatus::Idle | SessionStatus::Done) {
                return err(
                    id,
                    ERR_FAILED_PRECONDITION,
                    format!("session_send not allowed in state {:?}", state.status),
                    "failed_precondition",
                );
            }
            // Starting a new turn should not leak any unread terminal/error
            // steps from a prior turn into the next read.
            state.pending_steps.clear();
            state.status = SessionStatus::Streaming;
            (Arc::clone(&state.engine), Arc::clone(&state.notify))
        };

        let input = params.get("input").cloned().unwrap_or(Value::Null);
        if let Err(message) = engine.send(input).await {
            push_error_done(&session, &message).await;
        }
        notify.notify_waiters();

        ok(id, json!({}))
    }

    pub async fn read(&self, id: Value, params: Value) -> Value {
        const READ_BLOCK_TIMEOUT: Duration = Duration::from_secs(20);

        let Some(session_id) = params.get("session_id").and_then(Value::as_str) else {
            return err(
                id,
                ERR_INVALID_PARAMS,
                "missing session_id",
                "invalid_argument",
            );
        };

        let Some(entry) = self.sessions.get(session_id) else {
            return err(
                id,
                ERR_NOT_FOUND,
                format!("unknown session_id: {session_id}"),
                "not_found",
            );
        };

        let session = Arc::clone(entry.value());
        drop(entry);

        let deadline = Instant::now() + READ_BLOCK_TIMEOUT;
        loop {
            let (engine, status, notified) = {
                let mut state = session.lock().await;
                state.last_activity = Instant::now();

                if let Some(step) = state.pending_steps.pop_front() {
                    state.hop = state.hop.saturating_add(1);
                    if state.status == SessionStatus::AbortedPendingRead {
                        state.status = SessionStatus::Aborted;
                    }
                    let output =
                        build_output(state.hop, &step.events, step.completion, step.status);
                    return ok(id, json!({ "step": step.step, "output": output }));
                }

                if matches!(state.status, SessionStatus::Aborted | SessionStatus::Done) {
                    return ok(
                        id,
                        json!({
                            "step": "done",
                            "output": build_output(state.hop, &[], None, "done")
                        }),
                    );
                }

                let notify = Arc::clone(&state.notify);
                (
                    Arc::clone(&state.engine),
                    state.status,
                    notify.notified_owned(),
                )
            };

            if status == SessionStatus::Streaming {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return ok(
                        id,
                        json!({
                            "step": "streaming",
                            "output": build_output(0, &[], None, "streaming")
                        }),
                    );
                }

                match engine.read_next(remaining).await {
                    Ok(Some(ClaudeEvent::Streaming(batch))) => {
                        let mut state = session.lock().await;
                        state.hop = state.hop.saturating_add(1);
                        let output = build_output(state.hop, &batch, None, "streaming");
                        return ok(id, json!({ "step": "streaming", "output": output }));
                    }
                    Ok(Some(ClaudeEvent::TerminalDone(batch))) => {
                        let mut state = session.lock().await;
                        state.status = SessionStatus::Done;
                        state.hop = state.hop.saturating_add(1);
                        let output = build_output(state.hop, &batch, Some("DONE"), "done");
                        return ok(id, json!({ "step": "done", "output": output }));
                    }
                    Ok(None) => {
                        return ok(
                            id,
                            json!({
                                "step": "streaming",
                                "output": build_output(0, &[], None, "streaming")
                            }),
                        );
                    }
                    Err(message) => {
                        push_error_done(&session, &message).await;
                        continue;
                    }
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return ok(
                    id,
                    json!({
                        "step": "streaming",
                        "output": build_output(0, &[], None, "streaming")
                    }),
                );
            }
            let _ = tokio::time::timeout(remaining, notified).await;
        }
    }

    pub async fn finish(&self, id: Value, params: Value) -> Value {
        let Some(session_id) = params.get("session_id").and_then(Value::as_str) else {
            return err(
                id,
                ERR_INVALID_PARAMS,
                "missing session_id",
                "invalid_argument",
            );
        };
        let Some((_, session)) = self.sessions.remove(session_id) else {
            return err(
                id,
                ERR_NOT_FOUND,
                format!("unknown session_id: {session_id}"),
                "not_found",
            );
        };

        let engine = { Arc::clone(&session.lock().await.engine) };
        let _ = engine.close().await;
        ok(id, json!({}))
    }

    pub async fn abort(&self, id: Value, params: Value) -> Value {
        let Some(session_id) = params.get("session_id").and_then(Value::as_str) else {
            return err(
                id,
                ERR_INVALID_PARAMS,
                "missing session_id",
                "invalid_argument",
            );
        };

        let Some(entry) = self.sessions.get(session_id) else {
            return err(
                id,
                ERR_NOT_FOUND,
                format!("unknown session_id: {session_id}"),
                "not_found",
            );
        };

        let session = Arc::clone(entry.value());
        drop(entry);

        let engine = {
            let mut state = session.lock().await;
            state.last_activity = Instant::now();
            match state.status {
                SessionStatus::Streaming | SessionStatus::Idle => {
                    state.status = SessionStatus::AbortedPendingRead;
                    state.pending_steps.clear();
                    state.pending_steps.push_back(PendingStep {
                        step: "done",
                        events: vec![json!({
                            "kind": "terminal_result",
                            "subtype": "interrupted",
                            "is_error": true,
                            "num_turns": 1,
                            "total_cost_usd": 0.0,
                            "result": "Session aborted by caller."
                        })],
                        completion: Some("INTERRUPTED"),
                        status: "done",
                    });
                    state.notify.notify_waiters();
                }
                SessionStatus::AbortedPendingRead
                | SessionStatus::Aborted
                | SessionStatus::Done => {}
            }
            Arc::clone(&state.engine)
        };

        let _ = engine.close().await;
        ok(id, json!({}))
    }
}

async fn push_error_done(session: &Arc<Mutex<SessionState>>, message: &str) {
    let (subtype, completion) = if message.starts_with("UNAUTHENTICATED: ") {
        ("unauthenticated", "INTERRUPTED")
    } else if message.starts_with("UNAVAILABLE: ") {
        ("unavailable", "INTERRUPTED")
    } else {
        ("error", "INTERRUPTED")
    };

    let mut state = session.lock().await;
    state.last_activity = Instant::now();
    state.pending_steps.push_back(PendingStep {
        step: "done",
        events: vec![json!({
            "kind": "terminal_result",
            "subtype": subtype,
            "is_error": true,
            "num_turns": 1,
            "total_cost_usd": 0.0,
            "result": message,
        })],
        completion: Some(completion),
        status: "done",
    });
    state.status = SessionStatus::Done;
    state.notify.notify_waiters();
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::SessionStore;

    async fn open_session(store: &SessionStore) -> String {
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        }
        let open = store
            .open(
                json!(1),
                json!({
                    "tool_name": "dev/claude-ext"
                }),
            )
            .await;
        open.get("result")
            .and_then(|r| r.get("session_id"))
            .and_then(Value::as_str)
            .unwrap()
            .to_string()
    }

    async fn drain_until_done(store: &SessionStore, session_id: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let read = store
                .read(json!(0), json!({ "session_id": session_id }))
                .await;
            let step = read["result"]["step"].as_str().unwrap_or("");
            if step == "done" || step == "error" {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out draining session {session_id}; last read={read}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn send_after_done_is_allowed() {
        let store = SessionStore::default();
        let session_id = open_session(&store).await;

        let first = store
            .send(
                json!(2),
                json!({ "session_id": session_id.clone(), "input": {"prompt": "hello"} }),
            )
            .await;
        assert!(
            first.get("error").is_none(),
            "first send should succeed: {first}"
        );

        drain_until_done(&store, &session_id).await;

        let second = store
            .send(
                json!(3),
                json!({ "session_id": session_id.clone(), "input": {"prompt": "again"} }),
            )
            .await;

        assert!(
            second.get("error").is_none(),
            "second send after done should succeed: {second}"
        );

        drain_until_done(&store, &session_id).await;
    }

    #[tokio::test]
    async fn finish_removes_session_and_future_calls_are_not_found() {
        let store = SessionStore::default();
        let session_id = open_session(&store).await;

        let _ = store
            .finish(json!(2), json!({ "session_id": session_id.clone() }))
            .await;

        let read_after_finish = store
            .read(json!(3), json!({ "session_id": session_id }))
            .await;

        assert_eq!(
            read_after_finish["error"]["data"]["error_class"],
            "not_found"
        );
    }

    #[tokio::test]
    async fn abort_then_read_returns_interrupted_once_then_empty_done() {
        let store = SessionStore::default();
        let session_id = open_session(&store).await;

        let _ = store
            .abort(json!(3), json!({ "session_id": session_id.clone() }))
            .await;

        let first_read = store
            .read(json!(4), json!({ "session_id": session_id.clone() }))
            .await;
        assert_eq!(first_read["result"]["step"], "done");
        assert_eq!(first_read["result"]["output"]["completion"], "INTERRUPTED");

        let second_read = store
            .read(json!(5), json!({ "session_id": session_id }))
            .await;
        assert_eq!(second_read["result"]["step"], "done");
        assert_eq!(second_read["result"]["output"]["completion"], Value::Null);
        assert_eq!(second_read["result"]["output"]["events"], json!([]));
    }
}
