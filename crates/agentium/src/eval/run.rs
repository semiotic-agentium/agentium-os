// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Run eval cases against a deployed agent via A2A HTTP.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use console::style;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::manifest::{EvalCase, EvalManifest, TurnAssert};
use crate::{
    commands::{
        a2a_http::{
            AuthenticatedHttp, IngressPublishOutcome, SendStreamParams, create_eval_session,
            publish_ingress_fixture, send_message_stream,
        },
        utils::AgentPlatform,
    },
    project,
};

#[derive(Debug, Clone)]
pub struct EvalRunOptions {
    pub manifest_path: PathBuf,
    pub runner_url: Option<String>,
    pub model_override: Option<String>,
    pub min_pass_rate: f64,
    pub case_filter: Option<Vec<String>>,
    pub deploy: bool,
    pub agent_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct EvalSummary {
    pub run_id: String,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f64,
    pub cases: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub pass: bool,
    pub resolved_model: Option<String>,
    pub turns: Vec<TurnResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TurnResult {
    pub send: Option<String>,
    pub pass: bool,
    pub task_states: Vec<String>,
}

pub fn run_eval(
    opts: EvalRunOptions,
    runner_token: Option<crate::commands::utils::RunnerToken>,
) -> Result<EvalSummary> {
    let manifest = super::load_manifest(&opts.manifest_path)?;
    let (cfg, _) = project::discover_config(None)?;
    let base_url = opts
        .runner_url
        .clone()
        .unwrap_or_else(|| cfg.runner_base_url().to_string());
    let http = AgentPlatform::new(runner_token.clone())?;

    if opts.deploy {
        if let Some(path) = &opts.agent_path {
            crate::commands::install::install_agent(
                Some(&path.to_string_lossy()),
                None,
                None,
                Some(&base_url),
                "eval run",
                crate::commands::publish::PublishOriginArg::Iteration,
                runner_token,
            )?;
        }
    }

    let run_id = format!("run-{}", unix_now());
    let out_dir = PathBuf::from("eval/out").join(&run_id);
    std::fs::create_dir_all(&out_dir)?;

    let mut case_results = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;

    for case in &manifest.case {
        if let Some(filter) = &opts.case_filter {
            if !filter.iter().any(|id| id == &case.id) {
                continue;
            }
        }
        let agent = manifest
            .defaults
            .agent
            .clone()
            .or_else(|| cfg.project.default_agent.clone())
            .unwrap_or_else(|| "agent".to_string());
        let model = opts
            .model_override
            .clone()
            .or_else(|| case.model.clone())
            .or_else(|| manifest.defaults.model.clone());

        let auth = authenticated_http(&http, None);
        let session_id = http.block_on(create_eval_session(
            &auth,
            &base_url,
            &agent,
            model.as_deref(),
            case.client.as_deref(),
        ))?;

        match run_case(
            &http,
            &base_url,
            &agent,
            case,
            &manifest,
            session_id.as_deref(),
        ) {
            Ok(result) => {
                if result.pass {
                    passed += 1;
                } else {
                    failed += 1;
                }
                let path = out_dir.join(format!("{}.json", case.id));
                std::fs::write(&path, serde_json::to_string_pretty(&result)?)?;
                case_results.push(result);
            }
            Err(err) => {
                failed += 1;
                case_results.push(CaseResult {
                    id: case.id.clone(),
                    pass: false,
                    resolved_model: model,
                    turns: Vec::new(),
                    error: Some(format!("{err:#}")),
                });
            }
        }
    }

    let total = passed + failed;
    let pass_rate = if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    };
    let summary = EvalSummary {
        run_id,
        passed,
        failed,
        pass_rate,
        cases: case_results,
    };
    std::fs::write(
        out_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    std::fs::write(
        PathBuf::from("eval/out/last-run.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;

    println!(
        "{} {}/{} passed ({:.0}%)",
        style("[eval]").bold(),
        passed,
        total,
        pass_rate * 100.0
    );

    if pass_rate + f64::EPSILON < opts.min_pass_rate {
        bail!(
            "Eval pass rate {:.2}% below minimum {:.2}%",
            pass_rate * 100.0,
            opts.min_pass_rate * 100.0
        );
    }
    Ok(summary)
}

fn authenticated_http<'a>(
    http: &'a AgentPlatform,
    eval_session: Option<&'a str>,
) -> AuthenticatedHttp<'a> {
    AuthenticatedHttp {
        client: http.http_client(),
        runner_token: http.runner_token_ref(),
        eval_session,
    }
}

fn run_case(
    http: &AgentPlatform,
    base_url: &str,
    agent: &str,
    case: &EvalCase,
    manifest: &EvalManifest,
    eval_session: Option<&str>,
) -> Result<CaseResult> {
    let context_id = manifest
        .defaults
        .context_id
        .clone()
        .unwrap_or_else(|| format!("eval-{}", Uuid::new_v4()));

    let mut turn_results = Vec::new();
    let mut task_id: Option<String> = None;
    let mut all_pass = true;

    for turn in case.resolved_turns() {
        let mode = turn.mode.as_deref().unwrap_or(case.mode.as_str());
        if mode == "ingress" {
            let fixture = turn
                .fixture
                .as_ref()
                .context("ingress turn requires fixture")?;
            let raw = std::fs::read_to_string(fixture)
                .with_context(|| format!("Failed to read ingress fixture {fixture}"))?;
            let body: Value = serde_json::from_str(&raw)?;
            let auth = authenticated_http(http, eval_session);
            let outcome = http.block_on(publish_ingress_fixture(&auth, base_url, &body))?;
            let pass = check_ingress_assert(&turn.assert, &outcome)?;
            all_pass &= pass;
            turn_results.push(TurnResult {
                send: turn.send.clone(),
                pass,
                task_states: Vec::new(),
            });
            continue;
        }
        let send = turn.send.as_deref().unwrap_or("");
        let auth = authenticated_http(http, eval_session);
        let outcome = http.block_on(send_message_stream(
            &auth,
            &SendStreamParams {
                base_url,
                agent,
                instance: "default",
                context_id: &context_id,
                task_id: task_id.as_deref(),
                text: send,
                message_id: None,
                correlation_id: None,
            },
        ))?;
        if outcome.task_id.is_some() {
            task_id = outcome.task_id;
        }
        let pass = check_turn_assert(&turn.assert, &outcome.states, &outcome.texts);
        all_pass &= pass;
        turn_results.push(TurnResult {
            send: turn.send.clone(),
            pass,
            task_states: outcome.states,
        });
    }

    Ok(CaseResult {
        id: case.id.clone(),
        pass: all_pass,
        resolved_model: case
            .model
            .clone()
            .or_else(|| manifest.defaults.model.clone()),
        turns: turn_results,
        error: None,
    })
}

fn check_turn_assert(assert: &TurnAssert, states: &[String], texts: &[String]) -> bool {
    if let Some(expected) = &assert.task_states {
        for st in expected {
            let needle = st.trim_start_matches("TASK_STATE_");
            if !states.iter().any(|s| s.contains(needle)) {
                return false;
            }
        }
    }
    if let Some(forbidden) = &assert.not_task_states {
        for st in forbidden {
            let needle = st.trim_start_matches("TASK_STATE_");
            if states.iter().any(|s| s.contains(needle)) {
                return false;
            }
        }
    }
    check_text_assert(assert, &texts.join("\n"))
}

fn check_ingress_assert(assert: &TurnAssert, outcome: &IngressPublishOutcome) -> Result<bool> {
    if assert.artifact.is_some() {
        bail!("ingress turns do not support `assert.artifact`");
    }
    if assert.max_llm_calls.is_some() {
        bail!("ingress turns do not support `assert.max_llm_calls`");
    }
    if assert.task_states.is_some() || assert.not_task_states.is_some() {
        bail!(
            "ingress turns do not support task_state asserts; use contains/not_contains on the publish response"
        );
    }
    if !outcome.response.failures.is_empty() {
        return Ok(false);
    }
    Ok(check_text_assert(assert, &outcome.response_text))
}

fn check_text_assert(assert: &TurnAssert, haystack: &str) -> bool {
    let joined = haystack.to_lowercase();
    if let Some(needles) = &assert.contains {
        for needle in needles {
            if !joined.contains(&needle.to_lowercase()) {
                return false;
            }
        }
    }
    if let Some(needles) = &assert.not_contains {
        for needle in needles {
            if joined.contains(&needle.to_lowercase()) {
                return false;
            }
        }
    }
    true
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn init_eval_manifest(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sample = include_str!("../../eval/cases.toml.example");
    std::fs::write(path, sample)?;
    println!("Created {}", path.display());
    Ok(())
}

pub fn report_last_run() -> Result<()> {
    let path = PathBuf::from("eval/out/last-run.json");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("No last run at {}", path.display()))?;
    println!("{raw}");
    Ok(())
}
