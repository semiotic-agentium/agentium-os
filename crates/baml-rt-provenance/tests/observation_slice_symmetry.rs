//! Observation slice symmetry: one loader, aligned ops/episode counts, fingerprint invalidation.

use std::sync::Arc;

use baml_rt_conversation::{episode::StepType, view::ConversationItemContent};
use baml_rt_core::{
    Outcome,
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    CallScope, LlmUsage, ObservationLoader as _, ObservationScope, OpsQueryMode, PlanStepSpec,
    ProvEvent, ProvenanceOpsQueryRequest, ProvenanceOpsResource, ProvenanceWriter,
    SurrealStoreBuilder, TemporalBound, episode::EpisodeReader, events::TaskScopedEvent,
    observation_version_from_loaded, store::ProvenanceOpsFilters,
};

async fn bootstrap_task(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
) {
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            baml_rt_provenance::AgentType::new("symmetry-agent").expect("type"),
            "1.0.0".to_string(),
            "symmetry@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
}

fn task_llm_event(
    ctx: ContextId,
    task_id: TaskId,
    agent_id: &AgentId,
    anchor: &str,
    function_name: &str,
    outcome: Outcome,
    ts: u64,
) -> ProvEvent {
    ProvEvent::Task(TaskScopedEvent {
        id: ActivityAnchorId::from(anchor.to_string()),
        context_id: ctx.clone(),
        task_id: task_id.clone(),
        timestamp_ms: ts,
        data: baml_rt_provenance::events::ProvEventData::LlmCallCompleted {
            scope: CallScope::Task {
                task_id: task_id.clone(),
            },
            client: "openai".to_string(),
            model: "gpt-test".to_string(),
            function_name: function_name.to_string(),
            prompt: serde_json::json!({"q":"x"}),
            metadata: serde_json::json!({
                "agent_id": agent_id.as_str(),
                "task_id": task_id.as_str(),
            }),
            usage: LlmUsage::Known {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: None,
            },
            duration_ms: 100,
            outcome,
            drift: None,
            citations: vec![],
            resolved_citations: vec![],
            prompt_serialized_utf8_bytes: 2,
            prompt_message_chars: 1,
        },
    })
}

#[tokio::test]
async fn observation_slice_symmetry() {
    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("store"),
    );
    let ctx = ContextId::new(1_900_000_000_100, 1);
    let task_id = TaskId::from_external(ExternalId::new("dispatch-unit-symmetry"));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000088").unwrap());

    bootstrap_task(&store, &ctx, &task_id, &agent_id).await;

    for (i, name) in ["Fn1", "Fn2", "Fn3", "Fn4"].iter().enumerate() {
        store
            .add_event(task_llm_event(
                ctx.clone(),
                task_id.clone(),
                &agent_id,
                &format!("llm-ok-{i}"),
                name,
                Outcome::Success,
                100 + i as u64,
            ))
            .await
            .expect("llm ok");
    }

    store
        .add_event(task_llm_event(
            ctx.clone(),
            task_id.clone(),
            &agent_id,
            "llm-fail-1",
            "FailFn",
            Outcome::Failure,
            200,
        ))
        .await
        .expect("llm fail");

    store
        .add_event(ProvEvent::intent_resolved(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "Process ingress".to_string(),
            vec![],
            None,
            None,
        ))
        .await
        .expect("intent");

    store
        .add_event(ProvEvent::plan_generated(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "plan-1",
            vec![
                PlanStepSpec {
                    step_id: "s1".into(),
                    description: "Retrieve".to_string(),
                    order: 1,
                    depends_on: vec![],
                },
                PlanStepSpec {
                    step_id: "s2".into(),
                    description: "Abort step".to_string(),
                    order: 2,
                    depends_on: vec![],
                },
            ],
            None,
        ))
        .await
        .expect("plan");

    let scope = ObservationScope::for_task(ctx.clone(), task_id.clone(), None, TemporalBound::All);

    let loaded = store.load(scope.clone()).await.expect("load");
    assert_eq!(
        loaded.llm_call_count(),
        5,
        "counts success + failed LLM via TASK_CALL"
    );

    let fp_before = observation_version_from_loaded(&loaded)
        .as_str()
        .to_string();

    let ops = store
        .query_ops(OpsQueryMode::ContextScoped {
            scope: scope.clone(),
            request: ProvenanceOpsQueryRequest {
                resource: ProvenanceOpsResource::LlmCalls,
                filters: ProvenanceOpsFilters {
                    context_id: Some(ctx.clone()),
                    task_id: Some(task_id.clone()),
                    ..Default::default()
                },
                page_size: Some(100),
                ..Default::default()
            },
        })
        .await
        .expect("ops");
    let ops_count = ops.summary.count;
    assert_eq!(ops_count, 5, "ops summary aligned with slice LLM count");

    let episode = EpisodeReader::new(Arc::clone(&store))
        .read_snapshot(&ctx, &task_id)
        .await
        .expect("episode");
    assert_eq!(
        episode.token_summary.llm_call_count, 5,
        "episode token summary aligned with slice"
    );

    assert!(
        episode
            .transcript
            .iter()
            .any(|e| e.step_type == StepType::OperationalEvent),
        "episode transcript includes operational rows"
    );
    let episode_text = baml_rt_provenance::render_episode(&episode);
    assert!(
        episode_text.contains("operational kind=llm_call_failed"),
        "episode plain text includes LLM failure operational row"
    );
    assert!(
        !episode
            .session_history
            .iter()
            .any(|line| line.content.contains("llm_call_failed")),
        "session_history must not include operational diagnostics for BAML"
    );

    assert!(
        loaded.transcript.iter().any(|i| matches!(
            &i.content,
            ConversationItemContent::Operational(op)
                if matches!(
                    op.kind,
                    baml_rt_conversation::operational::OperationalEventKind::LlmCallFailed
                )
        )),
        "transcript includes failed LLM operational row"
    );
    assert!(
        loaded
            .transcript
            .iter()
            .any(|i| matches!(&i.content, ConversationItemContent::Planning(_))),
        "transcript includes planning rows"
    );

    store
        .add_event(ProvEvent::plan_step_status_changed(
            ctx.clone(),
            task_id.clone(),
            "intent-1",
            "plan-1",
            "s2",
            Some("aborted".to_string()),
            "failed".to_string(),
            vec![],
        ))
        .await
        .expect("step change");

    let loaded_after = store.load(scope).await.expect("load after step");
    let fp_after = observation_version_from_loaded(&loaded_after)
        .as_str()
        .to_string();
    assert_ne!(
        fp_before, fp_after,
        "planning step revision must change observation fingerprint"
    );
}
