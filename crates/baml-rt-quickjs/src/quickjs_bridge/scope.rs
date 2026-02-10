use baml_rt_core::context::RuntimeScope;
use quickjs_runtime::values::JsValueFacade;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

/// Opaque token issued by the host for the duration of an invocation. JS receives only this
/// string; natives look up scope by token so JS cannot forge attribution. See
/// docs/QUICKJS_THREADING_AND_SCOPE.md.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationToken(pub(crate) String);

static INVOCATION_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_invocation_token() -> InvocationToken {
    let n = INVOCATION_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationToken(format!("inv-{}", n))
}

pub(crate) fn token_from_args(args: &[JsValueFacade]) -> Option<InvocationToken> {
    args.first().and_then(|first| {
        if !first.is_string() {
            return None;
        }
        let token = first.get_str().to_string();
        if token.is_empty() {
            None
        } else {
            Some(InvocationToken(token))
        }
    })
}

/// Resolve invocation scope from native args: first arg must be a non-empty token string;
/// look up scope in map. Returns (scope, skip_count) where skip_count is 1 (token consumed).
pub(crate) fn resolve_scope_from_token_arg(
    map: &Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>,
    args: &[JsValueFacade],
) -> std::result::Result<(RuntimeScope, usize), quickjs_runtime::jsutils::JsError> {
    if !args.is_empty()
        && let Some(token_js) = args.first()
        && token_js.is_string()
    {
        let s = token_js.get_str().to_string();
        if !s.is_empty() {
            if let Ok(guard) = map.lock()
                && let Some(scope) = guard.get(&InvocationToken(s))
            {
                return Ok((scope.clone(), 1));
            }
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "Invalid or expired invocation token",
            ));
        }
    }
    Err(quickjs_runtime::jsutils::JsError::new_str(
        "Missing or invalid invocation token (bind a token in the eval scope or pass it explicitly to __tool_invoke/__baml_invoke/__baml_stream)",
    ))
}

pub(crate) async fn run_eval_with_scope(
    runtime: &quickjs_runtime::facades::QuickJsRuntimeFacade,
    scope: &baml_rt_core::context::InvocationScope,
    script: quickjs_runtime::jsutils::Script,
    clear_after: bool,
) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
    let _ = scope;
    let _ = clear_after;
    runtime.eval(None, script).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
    use proptest::prelude::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex as StdMutex};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(8))]

        #[test]
        fn prop_invocation_tokens_unique(n in 1u32..64u32) {
            let tokens: HashSet<_> = (0..n).map(|_| next_invocation_token()).collect();
            assert_eq!(
                tokens.len(),
                n as usize,
                "next_invocation_token must yield distinct tokens"
            );
        }
    }

    fn test_scope(seed: u64) -> RuntimeScope {
        RuntimeScope::task_scope(
            ContextId::new(1700000000000 + seed, seed),
            AgentId::from_uuid(
                UuidId::parse_str("00000000-0000-0000-0000-000000000777").expect("valid test uuid"),
            ),
            MessageId::from_external(ExternalId::new(format!("msg-{seed}"))),
            TaskId::from_external(ExternalId::new(format!("task-{seed}"))),
        )
    }

    #[test]
    fn token_resolution_rejects_expired_token_without_worker_fallback() {
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let args = vec![JsValueFacade::new_string("inv-expired".to_string())];

        let err = resolve_scope_from_token_arg(&map, &args).expect_err("expired token must fail");
        assert!(
            err.to_string()
                .contains("Invalid or expired invocation token"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn token_resolution_accepts_valid_token_map_entry() {
        let token_scope = test_scope(1);
        let token = InvocationToken("inv-123".to_string());
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        map.lock()
            .expect("map lock")
            .insert(token.clone(), token_scope.clone());

        let args = vec![JsValueFacade::new_string(token.0.clone())];
        let (resolved, skip) =
            resolve_scope_from_token_arg(&map, &args).expect("valid token resolves");

        assert_eq!(skip, 1, "token arg should be consumed");
        assert_eq!(resolved.context_id(), token_scope.context_id());
        assert_eq!(resolved.message_id(), token_scope.message_id());
        assert_eq!(resolved.task_id_opt(), token_scope.task_id_opt());
    }

    #[test]
    fn token_resolution_rejects_missing_token() {
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));

        let err = resolve_scope_from_token_arg(&map, &[]).expect_err("missing token must fail");
        assert!(
            err.to_string()
                .contains("Missing or invalid invocation token"),
            "unexpected error: {err:?}"
        );
    }
}
