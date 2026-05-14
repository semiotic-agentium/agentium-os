//! Step-executor hop graph deltas: **provenance-backed** `conversation_context` line changes
//! (strict prefix extension or set-delta) for composing `ctx.tags['conversation_history']` /
//! `ctx.tags['conversation_transcript']` with a loop-local `Vec<serde_json::Value>` that augments
//! the provider when the graph
//! lags a hop. No second copy of history on [`BamlRuntimeState`].

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use baml_rt_core::{BamlFunctionId, Result, context};
use baml_rt_provenance::DEFAULT_LLM_CONTEXT_ITEM_CAP;
use baml_rt_tools::{CTX_TAG_SESSION_STEP_STABLE_PREFIX, SESSION_STEP_STABLE_PREFIX_VALUE};
use baml_types::BamlValue;
use serde_json::Value;
use tokio::sync::RwLock;

use super::BamlRuntimeManager;

/// True when `p_after` is `p_before` plus a suffix (line-wise equality on [`Value`].
fn take_prefix_growth(p_before: &[Value], p_after: &[Value]) -> Option<Vec<Value>> {
    if p_after.len() < p_before.len() {
        return None;
    }
    for (i, b) in p_before.iter().enumerate() {
        if p_after.get(i) != Some(b) {
            return None;
        }
    }
    Some(p_after[p_before.len()..].to_vec())
}

/// Capped provider projection first, then the loop supplement **in order**, appending
/// only lines whose [`Value`] is not already in the provider slice.
///
/// The graph row is authoritative: a `Value` already on the provider side must
/// not reappear from the supplement. With the awaitable
/// `ProvenanceEffectSubscriber` persisting before `emit` returns, the provider
/// read after each hop already reflects LLM-backed projection — the merge is
/// belt-and-braces against transient projection lag, not load-bearing.
/// Then tail to [`DEFAULT_LLM_CONTEXT_ITEM_CAP`].
pub(crate) fn append_intra_lines_to_provider_then_cap(
    mut prov: Vec<Value>,
    extra: impl IntoIterator<Item = Value>,
) -> Vec<Value> {
    for line in extra {
        if !prov.contains(&line) {
            prov.push(line);
        }
    }
    if prov.len() > DEFAULT_LLM_CONTEXT_ITEM_CAP {
        let take = prov.len() - DEFAULT_LLM_CONTEXT_ITEM_CAP;
        return prov.split_off(take);
    }
    prov
}

/// Append new graph line values for this completed hop to the step-executor loop’s local
/// supplement (used at the next `invoke` when merged with the provider).
pub(crate) fn append_step_intra_deltas(
    supplement: &mut Vec<Value>,
    p_before: &[Value],
    p_after: &[Value],
    phase_function: &BamlFunctionId,
) -> baml_rt_core::Result<()> {
    let deltas = hop_lines_from_provider_delta(p_before, p_after, phase_function)?;
    supplement.extend(deltas);
    Ok(())
}

/// New `conversation_context` row(s) for this hop from provider snapshots only (no
/// placeholder rows). Prefer strict prefix extension; if the graph grows in length
/// but projection updates an earlier line (so raw JSON `Value` equality is not a
/// prefix), take **line objects that appear in `p_after` but not in `p_before`** in
/// `p_after` order — still graph-backed, never invented locally.
fn hop_lines_from_provider_delta(
    p_before: &[Value],
    p_after: &[Value],
    phase_function: &BamlFunctionId,
) -> baml_rt_core::Result<Vec<Value>> {
    // Stable `#N` in history rows means a re-read can be byte-for-byte identical to
    // `p_before` when the graph has not yet appended a new projected row (or the last hop
    // produced no new conversation line, e.g. some Finishes). In that case there is
    // nothing to append to the step supplement for this hop.
    if p_after == p_before {
        return Ok(Vec::new());
    }
    match take_prefix_growth(p_before, p_after) {
        Some(suf) => Ok(suf),
        None => {
            // Not a strict tail-append (projection can in-place update or trim rows). New hop
            // content is the multiset-style delta: `Value` lines in `p_after` not in `p_before`
            // (as a set, by equality), in `p_after` order. Still only graph-sourced line objects.
            let before: HashSet<&Value> = p_before.iter().collect();
            let novel: Vec<Value> = p_after
                .iter()
                .filter(|v| !before.contains(*v))
                .cloned()
                .collect();
            if novel.is_empty() {
                return Err(baml_rt_core::BamlRtError::BamlRuntime(format!(
                    "step executor hop {phase_function}: conversation_context is not a prefix \
                     extension and added no new line values vs the pre-step snapshot; graph \
                     reordered or suppressed only"
                )));
            }
            Ok(novel)
        }
    }
}

/// Reads graph-backed `conversation_context` once after a hop and validates it against `p_before`
/// for [`hop_lines_from_provider_delta`] (strict prefix extension or multiset delta).
///
/// [`BusWithEffects`](baml_rt_core::bus::BusWithEffects) awaits
/// [`EffectSubscriberTier::Awaitable`](baml_rt_core::bus::EffectSubscriberTier::Awaitable)
/// subscribers — notably `ProvenanceEffectSubscriber`, which persists the LLM completion row
/// that conversation projection consumes — before
/// [`EffectEmitter::emit`](baml_rt_core::bus::EffectEmitter::emit) returns, so the provider read
/// after each [`invoke_function_with_intra`](BamlRuntimeManager::invoke_function_with_intra) sees
/// LLM-backed projection without wall-clock polling. Background subscribers (status updates,
/// SSE relays) run detached and don't gate this read.
///
/// [`crate::step_executor_loop::run_step_executor_loop`] still merges a loop-local supplement at
/// each invoke via [`BamlRuntimeManager::invoke_function_with_intra`] when line-level merge needs
/// rows before the next graph round-trip.
pub(crate) async fn read_provider_conversation_after_hop(
    manager: &Arc<RwLock<BamlRuntimeManager>>,
    scope: &context::RuntimeScope,
    p_before: &[Value],
    phase_function: &BamlFunctionId,
) -> baml_rt_core::Result<Vec<Value>> {
    let p_after = {
        let g = manager.read().await;
        g.read_provider_conversation_array(scope).await?
    };
    hop_lines_from_provider_delta(p_before, &p_after, phase_function)?;
    Ok(p_after)
}

impl BamlRuntimeManager {
    /// Provider conversation lines plus an explicit step-executor local supplement, as JSON
    /// line objects in the same order and cap policy as `ctx.tags['conversation_history']`
    /// (supplement rows that match a line already in the provider are **not** repeated).
    ///
    /// Pass an empty `step_intra_supplement` for provider-only (same as graph projection
    /// for normal invokes). `step_intra_supplement` is the loop-owned buffer from
    /// [`run_step_executor_loop`](crate::step_executor_loop::run_step_executor_loop).
    pub async fn merged_conversation_history_lines_json(
        &self,
        scope: &context::RuntimeScope,
        step_intra_supplement: &[Value],
    ) -> Result<Value> {
        let Some(ref exec) = self.state.executor else {
            return Ok(Value::Array(vec![]));
        };
        let prov = exec.provider_conversation_history_lines(scope).await?;
        let merged =
            append_intra_lines_to_provider_then_cap(prov, step_intra_supplement.iter().cloned());
        Ok(Value::Array(merged))
    }

    /// `conversation_history` BAML tags: graph provider + optional loop-local supplement
    /// (no duplicate `Value` lines vs the provider slice; same merge as
    /// `append_intra_lines_to_provider_then_cap` in this module).
    pub(in crate::baml) async fn build_conversation_context_tags_with_intra(
        &self,
        scope: &context::RuntimeScope,
        step_intra_supplement: &[Value],
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let Some(ref exec) = self.state.executor else {
            return Ok(None);
        };
        let prov = exec.provider_conversation_history_lines(scope).await?;
        let merged =
            append_intra_lines_to_provider_then_cap(prov, step_intra_supplement.iter().cloned());
        let mut tags = exec
            .tags_from_merged_conversation_lines(merged)?
            .unwrap_or_default();
        tags.insert(
            CTX_TAG_SESSION_STEP_STABLE_PREFIX.to_string(),
            BamlValue::String(SESSION_STEP_STABLE_PREFIX_VALUE.to_string()),
        );
        Ok(Some(tags))
    }

    /// Full projected graph lines for step-executor `p_before` / `p_after` (uncapped when the
    /// provider supports it) so strict prefix growth is not spuriously broken by a sliding tail cap.
    pub(crate) async fn read_provider_conversation_array(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Vec<Value>> {
        let Some(ref exec) = self.state.executor else {
            return Ok(vec![]);
        };
        exec.provider_conversation_history_lines_for_intra_dedup(scope)
            .await
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::BamlFunctionId;
    use serde_json::{Value as JsonValue, json};

    use super::{
        append_intra_lines_to_provider_then_cap, hop_lines_from_provider_delta, take_prefix_growth,
    };

    fn fid() -> BamlFunctionId {
        BamlFunctionId::base("F")
    }

    #[test]
    fn prefix_growth_suffix() {
        let a = [json!({"a":1})];
        let b = [json!({"a":1}), json!({"b":2})];
        let suf = take_prefix_growth(&a, &b).expect("prefix");
        assert_eq!(suf, vec![json!({"b":2})]);
    }

    #[test]
    fn append_intra_preserves_order() {
        let a = json!({"role":"a"});
        let b = json!({"role":"b"});
        let m = append_intra_lines_to_provider_then_cap(vec![a.clone()], [b.clone()]);
        assert_eq!(m, vec![a, b]);
    }

    #[test]
    fn append_skips_extra_when_same_value_as_provider() {
        let line = json!({"role":"x","content":"1"});
        let m = append_intra_lines_to_provider_then_cap(vec![line.clone()], [line]);
        assert_eq!(
            m.len(),
            1,
            "supplement does not repeat graph rows the provider already shows"
        );
    }

    #[test]
    fn identical_before_after_yields_zero_deltas() {
        let a: Vec<JsonValue> = vec![];
        let p_after = a.clone();
        let d = hop_lines_from_provider_delta(&a, &p_after, &fid()).expect("no supplement rows");
        assert!(d.is_empty());
    }

    #[test]
    fn set_delta_picks_new_line_value_when_not_strict_prefix() {
        let a = [json!({"a":1})];
        let b = [json!({"b":2})];
        let d = hop_lines_from_provider_delta(&a, &b, &fid()).expect("one novel line value");
        assert_eq!(d, vec![json!({"b":2})]);
    }

    #[test]
    fn reorder_or_suppress_only_is_err_if_no_new_values() {
        let a = [json!({"x":1}), json!({"y":2})];
        // Same multiset as `a` but reordered: no novel `Value` vs the set of lines in a.
        let p_after = [json!({"y":2}), json!({"x":1})];
        assert!(hop_lines_from_provider_delta(&a, &p_after, &fid()).is_err());
    }
}
