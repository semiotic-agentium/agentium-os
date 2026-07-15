// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, OnceLock, RwLock};

use crate::{
    config::{SemioticConfig, SemioticPolicy},
    denied_recent::DeniedRecentStore,
    gate_outcome::GateOutcomeStore,
    pending_auth::PendingGateAuthStore,
    schema::ParseArtifact,
    store::GroundingStore,
};

static GLOBAL_STORE: OnceLock<Arc<GroundingStore>> = OnceLock::new();
static GLOBAL_GATE_OUTCOMES: OnceLock<Arc<GateOutcomeStore>> = OnceLock::new();
static GLOBAL_DENIED_RECENT: OnceLock<Arc<DeniedRecentStore>> = OnceLock::new();
static GLOBAL_PENDING_AUTH: OnceLock<Arc<PendingGateAuthStore>> = OnceLock::new();
static GLOBAL_CONFIG: OnceLock<Arc<RwLock<SemioticConfig>>> = OnceLock::new();

fn config_handle() -> Arc<RwLock<SemioticConfig>> {
    GLOBAL_CONFIG
        .get_or_init(|| Arc::new(RwLock::new(SemioticConfig::default())))
        .clone()
}

/// Process-wide grounding artifact store (shared with interceptor + submitGrounding).
pub fn global_grounding_store() -> Arc<GroundingStore> {
    GLOBAL_STORE
        .get_or_init(|| Arc::new(GroundingStore::new()))
        .clone()
}

/// Submit a grounding artifact for the current task scope.
pub fn submit_grounding(
    scope: &baml_rt_core::context::RuntimeScope,
    artifact: ParseArtifact,
    plan_step_id: Option<String>,
) {
    global_grounding_store().submit(scope, artifact, plan_step_id);
}

pub fn global_gate_outcome_store() -> Arc<GateOutcomeStore> {
    GLOBAL_GATE_OUTCOMES
        .get_or_init(|| Arc::new(GateOutcomeStore::new()))
        .clone()
}

pub fn global_denied_recent_store() -> Arc<DeniedRecentStore> {
    GLOBAL_DENIED_RECENT
        .get_or_init(|| Arc::new(DeniedRecentStore::new()))
        .clone()
}

pub fn global_pending_gate_auth_store() -> Arc<PendingGateAuthStore> {
    GLOBAL_PENDING_AUTH
        .get_or_init(|| Arc::new(PendingGateAuthStore::new()))
        .clone()
}

/// Grant or deny tier-3 gate authorization after the user resumes an `InputRequired` turn.
pub fn resolve_pending_gate_authorization(
    scope: &baml_rt_core::context::RuntimeScope,
    user_text: &str,
) -> crate::pending_auth::ResumeAction {
    global_pending_gate_auth_store().resolve_on_resume(scope, user_text)
}

/// Grant tier-3 gate authorization after the user resumes an `InputRequired` turn.
pub fn grant_pending_gate_authorization(scope: &baml_rt_core::context::RuntimeScope) {
    let _ = resolve_pending_gate_authorization(scope, "approve");
}

/// Current semiotic gate bundle (hot-reloaded from `PUT /config/semiotic`).
pub fn global_semiotic_config() -> crate::config::SemioticConfig {
    config_handle()
        .read()
        .expect("semiotic config lock")
        .clone()
}

/// Resolved policy for the executing agent package.
pub fn resolve_semiotic_policy(agent_package: Option<&str>) -> SemioticPolicy {
    global_semiotic_config().resolve(agent_package)
}

pub fn set_global_semiotic_config(config: crate::config::SemioticConfig) {
    *config_handle().write().expect("semiotic config lock") = config;
}
