// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Semiotic compiler gate — deterministic pre-action grounding (P4 policy).

pub mod config;
pub mod covers;
pub mod denied_recent;
pub mod gate;
pub mod gate_outcome;
pub mod global;
pub mod integrity;
pub mod interceptor;
pub mod lint;
pub mod pending_auth;
pub mod postcondition;
pub mod schema;
pub mod schema_export;
pub mod store;
pub mod store_loader;
pub mod telemetry;
pub mod tier;
pub mod trojan;

pub use config::{
    EffectiveAgentPolicy, EffectiveSystemPolicy, SEMIOTIC_CONFIG_BUNDLE_NAME, SemioticConfig,
    SemioticMode, SemioticOverrides, SemioticPolicy, SemioticPosture,
};
pub use gate::{AmbiguityAwareGate, GateAction, GateDecision, GatePolicy};
pub use gate_outcome::{GateOutcome, GateOutcomeStore};
pub use global::{
    global_denied_recent_store, global_gate_outcome_store, global_grounding_store,
    global_pending_gate_auth_store, global_semiotic_config, grant_pending_gate_authorization,
    resolve_pending_gate_authorization, resolve_semiotic_policy, set_global_semiotic_config,
    submit_grounding,
};
pub use integrity::{CitationIntegrityAssessment, CitationIntegrityEntry, IntegrityStatus};
pub use interceptor::{SemioticToolInterceptor, TrojanLintLLMInterceptor};
pub use pending_auth::ResumeAction;
pub use postcondition::{PostconditionKind, PostconditionRun};
pub use schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact, Template};
pub use schema_export::semiotic_bundle_schema;
pub use store_loader::load_stored_config;
pub use tier::{Tier, ToolTierMeta, classify_tier};
