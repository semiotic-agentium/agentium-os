//! Loaders for the seeded LGTM-style outage fixtures under
//! `tests/fixtures/incidents/`.
//!
//! These fixtures are used by:
//! - Ford/Grafana MCP investigation agent (issue #515).
//! - End-to-end rehearsal (issue #513) to assert the agent retrieved
//!   the expected evidence without brittle prose matching.
//! - Event Console exploratory dispatch (issues #519 / #520 / #526).
//!
//! The loaders intentionally model the dataset as plain JSON values:
//! they validate structural integrity without locking the schema to
//! Rust types that the downstream agent would also have to import.
//! Future code can layer typed views on top.
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

use crate::common::fixture_path;

/// Root directory containing all incident fixtures.
pub fn incidents_root() -> PathBuf {
    fixture_path("incidents")
}

/// Root directory for a single named scenario.
pub fn incident_scenario_root(scenario_id: &str) -> PathBuf {
    incidents_root().join(scenario_id)
}

/// Scenario manifest read from `scenario.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct IncidentScenario {
    pub fixture_version: String,
    pub scenario_id: String,
    pub title: String,
    pub affected_service: String,
    pub namespace: String,
    pub severity: String,
    pub time_range: ScenarioTimeRange,
    pub evidence_files: EvidenceFiles,
    pub event_samples: EventSamples,
    pub ground_truth_file: String,
    pub expected_evidence_file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioTimeRange {
    pub started_at: String,
    pub ended_at: String,
    pub alert_fired_at: String,
    pub alert_resolved_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvidenceFiles {
    pub metrics: Vec<String>,
    pub logs: Vec<String>,
    pub traces: Vec<String>,
    pub annotations: Vec<String>,
}

impl EvidenceFiles {
    /// All declared evidence file paths in family order: metrics, logs, traces, annotations.
    pub fn all_files(&self) -> impl Iterator<Item = &String> {
        self.metrics
            .iter()
            .chain(&self.logs)
            .chain(&self.traces)
            .chain(&self.annotations)
    }

    /// Primary evidence file paths (metrics, logs, traces) — annotations are supporting.
    /// `expected-evidence.json` must reference every primary file.
    pub fn primary_files(&self) -> impl Iterator<Item = &String> {
        self.metrics.iter().chain(&self.logs).chain(&self.traces)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventSamples {
    pub grafana_alert_firing: String,
    pub grafana_alert_resolved: String,
    pub firehydrant_incident_opened: String,
    pub manual_operator_exploratory: String,
}

/// A loaded scenario with paths resolved against its root.
#[derive(Debug, Clone)]
pub struct LoadedScenario {
    pub root: PathBuf,
    pub scenario: IncidentScenario,
}

impl LoadedScenario {
    /// Load and validate the named scenario from `tests/fixtures/incidents/<scenario_id>/`.
    pub fn load(scenario_id: &str) -> Result<Self, IncidentFixtureError> {
        let root = incident_scenario_root(scenario_id);
        let scenario: IncidentScenario = read_json(&root.join("scenario.json"))?;
        if scenario.scenario_id != scenario_id {
            return Err(IncidentFixtureError::IdMismatch {
                directory: scenario_id.to_string(),
                manifest: scenario.scenario_id,
            });
        }
        Ok(Self { root, scenario })
    }

    /// Absolute path to a scenario-relative file.
    pub fn resolve(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// All evidence file paths in declaration order: metrics, logs, traces, annotations.
    pub fn evidence_files(&self) -> Vec<PathBuf> {
        self.scenario
            .evidence_files
            .all_files()
            .map(|p| self.resolve(p))
            .collect()
    }

    /// All sample event payload paths.
    pub fn event_sample_files(&self) -> Vec<PathBuf> {
        let s = &self.scenario.event_samples;
        vec![
            self.resolve(&s.grafana_alert_firing),
            self.resolve(&s.grafana_alert_resolved),
            self.resolve(&s.firehydrant_incident_opened),
            self.resolve(&s.manual_operator_exploratory),
        ]
    }

    /// Load `expected-evidence.json` as a generic JSON value.
    pub fn expected_evidence(&self) -> Result<Value, IncidentFixtureError> {
        read_json(&self.resolve(&self.scenario.expected_evidence_file))
    }

    /// Load `ground-truth.json` as a generic JSON value.
    /// Test/reviewer use only — do not feed this to the agent prompt.
    pub fn ground_truth(&self) -> Result<Value, IncidentFixtureError> {
        read_json(&self.resolve(&self.scenario.ground_truth_file))
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, IncidentFixtureError> {
    let bytes = fs::read(path).map_err(|err| IncidentFixtureError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    serde_json::from_slice(&bytes).map_err(|err| IncidentFixtureError::Json {
        path: path.to_path_buf(),
        source: err,
    })
}

/// Reject any value that is not an RFC3339 UTC timestamp of the form
/// `YYYY-MM-DDTHH:MM:SSZ`. Strings shaped this way sort lexicographically
/// in time order, so callers can compare them as `&str` directly.
fn check_utc_rfc3339(value: &str) -> Result<(), IncidentFixtureError> {
    let bytes = value.as_bytes();
    let shape_ok = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z';
    let digits_ok = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
        .iter()
        .all(|&i| bytes.get(i).is_some_and(u8::is_ascii_digit));
    if shape_ok && digits_ok {
        Ok(())
    } else {
        Err(IncidentFixtureError::Timestamp(value.to_string()))
    }
}

/// Validate cross-file integrity of a loaded scenario:
///
/// - Every declared evidence file and event sample exists and is valid JSON.
/// - Scenario timestamps parse as `YYYY-MM-DDTHH:MM:SSZ` and are ordered
///   `started_at <= alert_fired_at <= alert_resolved_at <= ended_at`.
/// - Every evidence file declares a `time_range` whose start is on or after the scenario start
///   and whose end is on or before the scenario end. (Annotations may omit a `time_range`.)
/// - Every event sample carries `scenario_ref.scenario_id` matching this scenario
///   and a non-empty `subscription_hint.schema_versions`.
/// - `expected-evidence.json` references only evidence files declared in `scenario.json`,
///   and every metrics/logs/traces evidence file declared in `scenario.json` is referenced
///   by at least one entry of `expected-evidence.json`.
pub fn validate_scenario(scenario: &LoadedScenario) -> Result<(), IncidentFixtureError> {
    let started = &scenario.scenario.time_range.started_at;
    let ended = &scenario.scenario.time_range.ended_at;
    let alert_fired = &scenario.scenario.time_range.alert_fired_at;
    let alert_resolved = &scenario.scenario.time_range.alert_resolved_at;
    for value in [started, ended, alert_fired, alert_resolved] {
        check_utc_rfc3339(value)?;
    }
    if !(started.as_str() <= alert_fired.as_str()
        && alert_fired.as_str() <= alert_resolved.as_str()
        && alert_resolved.as_str() <= ended.as_str())
    {
        return Err(IncidentFixtureError::TimelineOrder {
            started_at: started.clone(),
            alert_fired_at: alert_fired.clone(),
            alert_resolved_at: alert_resolved.clone(),
            ended_at: ended.clone(),
        });
    }

    let declared_evidence: HashSet<&str> = scenario
        .scenario
        .evidence_files
        .all_files()
        .map(String::as_str)
        .collect();

    for relative in &declared_evidence {
        let path = scenario.resolve(relative);
        let body: Value = read_json(&path)?;
        if let Some(range) = body.get("time_range") {
            let body_start = range.get("start").and_then(Value::as_str).ok_or_else(|| {
                IncidentFixtureError::EvidenceShape {
                    path: path.clone(),
                    detail: "time_range.start missing".to_string(),
                }
            })?;
            let body_end = range.get("end").and_then(Value::as_str).ok_or_else(|| {
                IncidentFixtureError::EvidenceShape {
                    path: path.clone(),
                    detail: "time_range.end missing".to_string(),
                }
            })?;
            check_utc_rfc3339(body_start)?;
            check_utc_rfc3339(body_end)?;
            if body_start < started.as_str() || body_end > ended.as_str() {
                return Err(IncidentFixtureError::EvidenceWindow {
                    path: path.clone(),
                    body_start: body_start.to_string(),
                    body_end: body_end.to_string(),
                    scenario_start: started.clone(),
                    scenario_end: ended.clone(),
                });
            }
        }
    }

    for path in scenario.event_sample_files() {
        let body: Value = read_json(&path)?;
        let ref_id = body
            .get("scenario_ref")
            .and_then(|v| v.get("scenario_id"))
            .and_then(Value::as_str);
        match ref_id {
            Some(id) if id == scenario.scenario.scenario_id => {}
            other => {
                return Err(IncidentFixtureError::EventScenarioRef {
                    path,
                    found: other.map(str::to_string),
                    expected: scenario.scenario.scenario_id.clone(),
                });
            }
        }
        if body
            .get("subscription_hint")
            .and_then(|v| v.get("schema_versions"))
            .and_then(Value::as_array)
            .is_none_or(|arr| arr.is_empty())
        {
            return Err(IncidentFixtureError::EventShape {
                path,
                detail: "subscription_hint.schema_versions missing or empty".to_string(),
            });
        }
    }

    let expected_evidence = scenario.expected_evidence()?;
    let entries = expected_evidence
        .get("evidence")
        .and_then(Value::as_array)
        .ok_or(IncidentFixtureError::ExpectedEvidenceShape(
            "evidence array missing",
        ))?;
    let mut referenced_files: HashSet<&str> = HashSet::new();
    for entry in entries {
        let file = entry.get("file").and_then(Value::as_str).ok_or(
            IncidentFixtureError::ExpectedEvidenceShape("evidence[].file missing"),
        )?;
        if !declared_evidence.contains(file) {
            return Err(IncidentFixtureError::ExpectedEvidenceUnknownFile(
                file.to_string(),
            ));
        }
        referenced_files.insert(file);
    }
    for required in scenario.scenario.evidence_files.primary_files() {
        if !referenced_files.contains(required.as_str()) {
            return Err(IncidentFixtureError::ExpectedEvidenceUnreferenced(
                required.clone(),
            ));
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum IncidentFixtureError {
    #[error("scenario id mismatch: directory {directory} but manifest {manifest}")]
    IdMismatch { directory: String, manifest: String },
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("timestamp {0} is not RFC3339 UTC (YYYY-MM-DDTHH:MM:SSZ)")]
    Timestamp(String),
    #[error(
        "scenario timeline not ordered: started_at={started_at}, alert_fired_at={alert_fired_at}, alert_resolved_at={alert_resolved_at}, ended_at={ended_at}"
    )]
    TimelineOrder {
        started_at: String,
        alert_fired_at: String,
        alert_resolved_at: String,
        ended_at: String,
    },
    #[error("evidence file {path} is malformed: {detail}")]
    EvidenceShape { path: PathBuf, detail: String },
    #[error(
        "evidence file {path} time window [{body_start}, {body_end}] is outside scenario window [{scenario_start}, {scenario_end}]"
    )]
    EvidenceWindow {
        path: PathBuf,
        body_start: String,
        body_end: String,
        scenario_start: String,
        scenario_end: String,
    },
    #[error("event sample {path} has scenario_ref.scenario_id={found:?}, expected {expected}")]
    EventScenarioRef {
        path: PathBuf,
        found: Option<String>,
        expected: String,
    },
    #[error("event sample {path} is malformed: {detail}")]
    EventShape { path: PathBuf, detail: String },
    #[error("expected-evidence.json is malformed: {0}")]
    ExpectedEvidenceShape(&'static str),
    #[error("expected-evidence.json references unknown evidence file: {0}")]
    ExpectedEvidenceUnknownFile(String),
    #[error(
        "evidence file {0} is declared by scenario.json but never referenced by expected-evidence.json"
    )]
    ExpectedEvidenceUnreferenced(String),
}
