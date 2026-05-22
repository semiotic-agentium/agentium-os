//! Integrity tests for the seeded LGTM-style outage fixture under
//! `tests/fixtures/incidents/lgtm-checkout-5xx/`.

use std::collections::HashSet;

use serde_json::Value;
use test_support::incident_fixtures::{LoadedScenario, read_json, validate_scenario};

const SCENARIO_ID: &str = "lgtm-checkout-5xx";

fn load_event(scenario: &LoadedScenario, relative: &str) -> Value {
    read_json::<Value>(&scenario.resolve(relative))
        .unwrap_or_else(|err| panic!("read {relative}: {err}"))
}

#[test]
fn lgtm_checkout_5xx_validates() {
    let loaded = LoadedScenario::load(SCENARIO_ID).expect("load lgtm-checkout-5xx scenario");
    validate_scenario(&loaded).expect("scenario integrity");
}

#[test]
fn lgtm_checkout_5xx_has_three_telemetry_families() {
    let loaded = LoadedScenario::load(SCENARIO_ID).expect("load scenario");
    assert!(
        !loaded.scenario.evidence_files.metrics.is_empty(),
        "scenario must seed metrics evidence"
    );
    assert!(
        !loaded.scenario.evidence_files.logs.is_empty(),
        "scenario must seed logs evidence"
    );
    assert!(
        !loaded.scenario.evidence_files.traces.is_empty(),
        "scenario must seed traces evidence"
    );
}

#[test]
fn lgtm_checkout_5xx_event_samples_cover_three_families() {
    let loaded = LoadedScenario::load(SCENARIO_ID).expect("load scenario");
    let samples = &loaded.scenario.event_samples;

    let firing = load_event(&loaded, &samples.grafana_alert_firing);
    assert_eq!(
        firing["schema_version"].as_str(),
        Some("grafana.alert.v1"),
        "grafana firing must use grafana.alert.v1 schema"
    );
    assert_eq!(
        firing["payload"]["status"].as_str(),
        Some("firing"),
        "grafana firing payload status must be 'firing'"
    );

    let resolved = load_event(&loaded, &samples.grafana_alert_resolved);
    assert_eq!(
        resolved["payload"]["status"].as_str(),
        Some("resolved"),
        "grafana resolved payload status must be 'resolved'"
    );

    let firehydrant = load_event(&loaded, &samples.firehydrant_incident_opened);
    assert_eq!(
        firehydrant["fixture_only"].as_bool(),
        Some(true),
        "firehydrant sample must be marked fixture_only"
    );
    assert_eq!(
        firehydrant["payload"]["type"].as_str(),
        Some("incident.opened")
    );

    let manual = load_event(&loaded, &samples.manual_operator_exploratory);
    assert_eq!(
        manual["schema_version"].as_str(),
        Some("incident.manual.v1")
    );
    assert!(
        manual["payload"]["time_range"]["start"].is_string(),
        "manual sample must declare a time_range.start"
    );
}

#[test]
fn lgtm_checkout_5xx_expected_evidence_covers_root_cause_terms() {
    let loaded = LoadedScenario::load(SCENARIO_ID).expect("load scenario");
    let expected = loaded.expected_evidence().expect("expected evidence");
    let terms: HashSet<&str> = expected["expected_root_cause_terms_any_of"]
        .as_array()
        .expect("root cause terms array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for must_have in ["payments-gateway", "v2.34.0", "200ms"] {
        assert!(
            terms.iter().any(|t| t.contains(must_have)),
            "expected_root_cause_terms_any_of must mention {must_have} (got {terms:?})"
        );
    }
}

#[test]
fn lgtm_checkout_5xx_ground_truth_warning_is_present() {
    let loaded = LoadedScenario::load(SCENARIO_ID).expect("load scenario");
    let ground_truth = loaded.ground_truth().expect("ground truth");
    let warning = ground_truth["warning"]
        .as_str()
        .expect("ground truth must carry a warning");
    assert!(
        warning.to_lowercase().contains("do not feed"),
        "ground-truth.json must warn against feeding to agent: {warning}"
    );
}
