//! Fixture-driven drift scoring tests.
//!
//! Parses TOML scenarios from `tests/fixtures/drift/*.toml`, embeds text
//! using the real GTE-base model, runs `score_plan_drift`, and asserts
//! the expected score bounds. These are empirical tests — they verify
//! that the embedding model + scoring pipeline produces the expected
//! severity classifications for curated scenarios.
//!
//! Marked `#[ignore]` because they require ~500MB model download on first
//! run. Execute with: `cargo test -p baml-rt-embedding -- drift_fixture --ignored`

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde::Deserialize;

    use crate::{
        BIPIA_CITE_MEAN_THRESHOLD, BIPIA_STEP_ALIGN_THRESHOLD, DriftSeverity,
        assessment::{CitationDriftAssessment, score_bipia_signal, score_citation_drift},
        plan_assessment::{
            PlanDriftAssessment, PlanDriftConfig, PlanDriftInputs, score_plan_drift,
        },
        provider::{EmbeddingProvider, FastEmbedProvider},
        similarity::cosine_similarity,
        trajectory::TaskDriftTracker,
    };

    #[derive(Debug, Deserialize)]
    struct FixtureFile {
        scenario: Vec<Scenario>,
    }

    #[derive(Debug, Deserialize)]
    struct Scenario {
        name: String,
        category: String,
        #[serde(default)]
        phase: Option<String>,
        #[serde(default)]
        description: Option<String>,
        intent: IntentSpec,
        #[serde(default)]
        plan: Option<PlanSpec>,
        #[serde(default)]
        step: Option<StepSpec>,
        response: ResponseSpec,
        expected: ExpectedSpec,
        #[serde(default)]
        prompt: Option<serde_json::Value>,
        /// Optional citation signals to score independently.
        #[serde(default)]
        citations: Vec<CitationSpec>,
        /// Optional assertions on the citation drift result.
        #[serde(default)]
        citation_expected: Option<CitationExpectedSpec>,
    }

    #[derive(Debug, Deserialize)]
    struct IntentSpec {
        description: String,
    }

    #[derive(Debug, Deserialize)]
    struct PlanSpec {
        objective: String,
        #[serde(default)]
        is_revised: bool,
    }

    #[derive(Debug, Deserialize)]
    struct StepSpec {
        step_id: String,
        description: String,
        order: u32,
    }

    #[derive(Debug, Deserialize)]
    struct ResponseSpec {
        value: String,
    }

    /// A single citation entry in the fixture.
    ///
    /// `content` is the resolved text of the cited history/archive entry.
    /// `negated` marks counter-evidence (`!#N` / `!@N` semantics).
    #[derive(Debug, Deserialize)]
    struct CitationSpec {
        n: u32,
        #[serde(default = "default_true")]
        is_history: bool,
        #[serde(default)]
        negated: bool,
        content: String,
    }

    fn default_true() -> bool { true }

    /// Assertions on the `CitationDriftAssessment` produced from `citations`.
    ///
    /// All fields are optional — only set the ones relevant to the scenario.
    /// Works even when `scenario.citations` is empty (coverage will be 0.0 in that case).
    #[derive(Debug, Deserialize)]
    struct CitationExpectedSpec {
        /// Minimum `mean_similarity` (positive citations only; 1.0 when no positive citations).
        #[serde(default)]
        mean_similarity_min: Option<f32>,
        /// Maximum `mean_similarity`.
        #[serde(default)]
        mean_similarity_max: Option<f32>,
        /// Minimum citation coverage (cited_decisions / total_decisions).
        /// Set to e.g. `0.99` to assert that at least one citation was provided.
        /// Set to `0.0` to assert the model cited nothing.
        #[serde(default)]
        coverage_min: Option<f32>,
        /// Maximum citation coverage.
        #[serde(default)]
        coverage_max: Option<f32>,
        /// `true` → the scenario must contain at least one negated citation.
        #[serde(default)]
        has_negated: Option<bool>,
        /// `true`  → the BIPIA firewall MUST fire  (step_align < τ₁ AND cite_mean > τ₂).
        /// `false` → the firewall MUST NOT fire.
        /// `None`  → no assertion on BIPIA signal.
        #[serde(default)]
        bipia_suspect: Option<bool>,
        /// Override τ₁ (step alignment threshold) for this scenario.
        /// Useful for synthesis steps where the step description is semantically broad
        /// and step_align naturally sits higher even for BIPIA injections.
        #[serde(default)]
        bipia_step_threshold: Option<f32>,
        /// Override τ₂ (cite_mean threshold) for this scenario.
        #[serde(default)]
        bipia_cite_threshold: Option<f32>,
    }

    #[derive(Debug, Deserialize)]
    struct ExpectedSpec {
        #[serde(default)]
        phase: Option<String>,
        #[serde(default)]
        intent_alignment_min: Option<f32>,
        #[serde(default)]
        intent_alignment_max: Option<f32>,
        #[serde(default)]
        step_alignment_min: Option<f32>,
        #[serde(default)]
        step_alignment_max: Option<f32>,
        #[serde(default)]
        step_alignment: Option<String>,
        #[serde(default)]
        trajectory_drift_min: Option<f32>,
        #[serde(default)]
        composite_severity: Option<String>,
    }

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests/fixtures/drift")
    }

    fn parse_severity(s: &str) -> DriftSeverity {
        match s {
            "acceptable" => DriftSeverity::Acceptable,
            "warn" => DriftSeverity::Warn,
            "block" => DriftSeverity::Block,
            other => panic!("unknown severity: {other}"),
        }
    }

    fn embed(provider: &dyn EmbeddingProvider, text: &str) -> Vec<f32> {
        provider
            .embed_batch(&[text])
            .expect("embedding failed")
            .into_iter()
            .next()
            .expect("empty embedding batch")
    }

    fn run_fixture_file(
        path: &std::path::Path,
        provider: &dyn EmbeddingProvider,
        reranker: &dyn crate::reranker::RerankProvider,
    ) {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let file: FixtureFile = match toml::from_str(&content) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "  SKIP {} (parse error: {e})",
                    path.file_name().unwrap().to_str().unwrap()
                );
                return;
            }
        };

        let config = PlanDriftConfig::default();
        let mut pass = 0;
        let mut fail = 0;

        for scenario in &file.scenario {
            let intent_emb = embed(provider, &scenario.intent.description);

            // Extract response text from the value — may be JSON or plain text.
            let response_text = crate::extraction::extract_response_text(
                &serde_json::Value::String(scenario.response.value.clone()),
            );
            let response_emb = embed(provider, &response_text);

            let mut tracker = TaskDriftTracker::new(intent_emb, config.ema_alpha);

            let is_pre_plan = scenario
                .phase
                .as_deref()
                .or(scenario.expected.phase.as_deref())
                == Some("pre_plan");

            let result = if is_pre_plan {
                let inputs = PlanDriftInputs::PrePlan {
                    response_embedding: &response_emb,
                    is_revised: false,
                };
                let tactical = crate::DriftAssessment {
                    score: 0.0,
                    severity: DriftSeverity::Acceptable,
                    mode: crate::DriftMode::Audit,
                    warn_min_score: config.intent_warn_min,
                    block_min_score: config.intent_block_min,
                    intent_text_preview: String::new(),
                    response_text_preview: String::new(),
                };
                score_plan_drift(&inputs, tactical, &mut tracker, &config)
            } else if let Some(ref step) = scenario.step {
                tracker.set_current_step(step.step_id.clone());
                let step_emb = embed(provider, &step.description);
                let is_revised = scenario
                    .plan
                    .as_ref()
                    .map(|p| p.is_revised)
                    .unwrap_or(false);
                let xe_score = reranker
                    .score_pair(&step.description, &response_text)
                    .unwrap_or(0.0);
                let inputs = PlanDriftInputs::WithStep {
                    step_embedding: &step_emb,
                    response_embedding: &response_emb,
                    step_index: step.order,
                    total_steps: 3,
                    is_revised,
                    cross_encoder_step_score: xe_score,
                };
                let tactical = crate::DriftAssessment {
                    score: 0.0,
                    severity: DriftSeverity::Acceptable,
                    mode: crate::DriftMode::Audit,
                    warn_min_score: config.intent_warn_min,
                    block_min_score: config.intent_block_min,
                    intent_text_preview: String::new(),
                    response_text_preview: String::new(),
                };
                score_plan_drift(&inputs, tactical, &mut tracker, &config)
            } else {
                eprintln!("  SKIP {}: no step and not pre_plan", scenario.name);
                continue;
            };

            // --- Citation drift scoring (independent of plan drift) ---
            // Also run when citations is empty but citation_expected is set, so
            // coverage assertions (coverage_min/coverage_max) can fire on missing citations.
            let citation_assessment: Option<CitationDriftAssessment> =
                if !scenario.citations.is_empty() {
                    let resolved: Vec<(u32, bool, bool, String)> = scenario
                        .citations
                        .iter()
                        .map(|c| (c.n, c.is_history, c.negated, c.content.clone()))
                        .collect();
                    score_citation_drift(&response_text, &resolved, 1, 1, provider)
                } else if scenario.citation_expected.is_some() {
                    // Empty citations with citation_expected present → report coverage=0.
                    score_citation_drift(&response_text, &[], 1, 0, provider)
                } else {
                    None
                };

            let mut errors = Vec::new();

            // Citation assertions + BIPIA firewall check
            if let (Some(ca), Some(ce)) = (&citation_assessment, &scenario.citation_expected) {
                if let Some(min) = ce.mean_similarity_min {
                    if ca.mean_similarity < min {
                        errors.push(format!(
                            "citation mean_similarity {:.3} < min {min}",
                            ca.mean_similarity
                        ));
                    }
                }
                if let Some(max) = ce.mean_similarity_max {
                    if ca.mean_similarity > max {
                        errors.push(format!(
                            "citation mean_similarity {:.3} > max {max}",
                            ca.mean_similarity
                        ));
                    }
                }
                if let Some(min) = ce.coverage_min {
                    if ca.coverage < min {
                        errors.push(format!(
                            "coverage {:.3} < min {min}",
                            ca.coverage
                        ));
                    }
                }
                if let Some(max) = ce.coverage_max {
                    if ca.coverage > max {
                        errors.push(format!(
                            "coverage {:.3} > max {max}",
                            ca.coverage
                        ));
                    }
                }
                if let Some(expect_negated) = ce.has_negated {
                    let found_negated = ca.per_citation.iter().any(|c| c.negated);
                    if found_negated != expect_negated {
                        errors.push(format!(
                            "has_negated expected {expect_negated}, got {found_negated}"
                        ));
                    }
                }
                // 2D BIPIA firewall check using step_alignment from plan drift result
                if let Some(expect_bipia) = ce.bipia_suspect {
                    let step_align = result.step_alignment().unwrap_or(1.0);
                    let tau1 = ce.bipia_step_threshold.or(Some(BIPIA_STEP_ALIGN_THRESHOLD));
                    let tau2 = ce.bipia_cite_threshold.or(Some(BIPIA_CITE_MEAN_THRESHOLD));
                    let bipia = score_bipia_signal(step_align, ca, tau1, tau2);
                    if bipia.flagged != expect_bipia {
                        errors.push(format!(
                            "bipia_suspect expected {expect_bipia}, got {} \
                             (step={:.3} < {:.2}={}, cite_mean={:.3} > {:.2}={}, pos_cites={})",
                            bipia.flagged,
                            bipia.step_alignment,
                            BIPIA_STEP_ALIGN_THRESHOLD,
                            bipia.step_alignment < BIPIA_STEP_ALIGN_THRESHOLD,
                            bipia.cite_mean,
                            BIPIA_CITE_MEAN_THRESHOLD,
                            bipia.cite_mean > BIPIA_CITE_MEAN_THRESHOLD,
                            bipia.positive_citation_count,
                        ));
                    }
                }
            }

            if let Some(min) = scenario.expected.intent_alignment_min {
                if result.intent_alignment() < min {
                    errors.push(format!(
                        "intent_alignment {:.3} < min {min}",
                        result.intent_alignment()
                    ));
                }
            }
            if let Some(max) = scenario.expected.intent_alignment_max {
                if result.intent_alignment() > max {
                    errors.push(format!(
                        "intent_alignment {:.3} > max {max}",
                        result.intent_alignment()
                    ));
                }
            }
            if let Some(min) = scenario.expected.step_alignment_min {
                match result.step_alignment() {
                    Some(sa) if sa < min => {
                        errors.push(format!("step_alignment {sa:.3} < min {min}"));
                    }
                    None => errors.push("step_alignment absent but min specified".to_string()),
                    _ => {}
                }
            }
            if let Some(max) = scenario.expected.step_alignment_max {
                match result.step_alignment() {
                    Some(sa) if sa > max => {
                        errors.push(format!("step_alignment {sa:.3} > max {max}"));
                    }
                    _ => {}
                }
            }
            if scenario.expected.step_alignment.as_deref() == Some("absent") {
                if result.step_alignment().is_some() {
                    errors.push("step_alignment should be absent".to_string());
                }
            }
            if let Some(min) = scenario.expected.trajectory_drift_min {
                if result.trajectory_drift() < min {
                    errors.push(format!(
                        "trajectory_drift {:.3} < min {min}",
                        result.trajectory_drift()
                    ));
                }
            }
            if let Some(ref expected_sev) = scenario.expected.composite_severity {
                let expected = parse_severity(expected_sev);
                if result.composite_severity() != expected {
                    errors.push(format!(
                        "composite_severity {:?} != expected {:?}",
                        result.composite_severity(),
                        expected
                    ));
                }
            }

            let xe_str = if let PlanDriftAssessment::PlanCommitted {
                cross_encoder_step_score,
                ..
            } = &result
            {
                format!(" xe={cross_encoder_step_score:.1}")
            } else {
                String::new()
            };

            let bipia_str = if let Some(ca) = &citation_assessment {
                let step_align = result.step_alignment().unwrap_or(1.0);
                let tau1 = scenario.citation_expected.as_ref()
                    .and_then(|ce| ce.bipia_step_threshold)
                    .unwrap_or(BIPIA_STEP_ALIGN_THRESHOLD);
                let tau2 = scenario.citation_expected.as_ref()
                    .and_then(|ce| ce.bipia_cite_threshold)
                    .unwrap_or(BIPIA_CITE_MEAN_THRESHOLD);
                let sig = score_bipia_signal(step_align, ca, Some(tau1), Some(tau2));
                if sig.flagged {
                    format!(" [BIPIA🚨 step={:.2}<{tau1} cite={:.2}>{tau2}]", sig.step_alignment, sig.cite_mean)
                } else {
                    format!(" [bipia_ok step={:.2} cite={:.2}]", sig.step_alignment, sig.cite_mean)
                }
            } else {
                String::new()
            };
            let cite_str = citation_assessment
                .as_ref()
                .map(|ca| format!(" cite_mean={:.2}", ca.mean_similarity))
                .unwrap_or_default();

            if errors.is_empty() {
                pass += 1;
                eprintln!(
                    "  PASS {}: intent={:.2} step={} traj={:.2}{xe_str}{cite_str}{bipia_str} sev={:?}",
                    scenario.name,
                    result.intent_alignment(),
                    result
                        .step_alignment()
                        .map(|s| format!("{s:.2}"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    result.trajectory_drift(),
                    result.composite_severity(),
                );
            } else {
                fail += 1;
                eprintln!(
                    "  FAIL {}: intent={:.2} step={} traj={:.2}{xe_str}{cite_str}{bipia_str} adh={:.2} sev={:?}",
                    scenario.name,
                    result.intent_alignment(),
                    result
                        .step_alignment()
                        .map(|s| format!("{s:.2}"))
                        .unwrap_or_else(|| "n/a".to_string()),
                    result.trajectory_drift(),
                    result.plan_adherence_score(),
                    result.composite_severity(),
                );
                for e in &errors {
                    eprintln!("    -> {e}");
                }
            }
        }

        eprintln!(
            "\n  {} results: {} passed, {} failed",
            path.file_name().unwrap().to_str().unwrap(),
            pass,
            fail
        );
        assert_eq!(
            fail,
            0,
            "{fail} fixture scenario(s) failed in {}",
            path.display()
        );
    }

    fn init_models() -> (FastEmbedProvider, crate::reranker::FastRerankProvider) {
        let provider = FastEmbedProvider::new().expect("embedding model init");
        let reranker = crate::reranker::FastRerankProvider::new().expect("reranker model init");
        (provider, reranker)
    }

    #[test]
    #[ignore]
    fn drift_fixture_01_aligned_execution() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("01_aligned_execution.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_03_prompt_injection() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("03_prompt_injection.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_09_tool_action_summaries() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("09_tool_action_summaries.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_12_synthesis_citations() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("12_synthesis_citations.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_11_bipia_firewall() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("11_bipia_firewall.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_10_citation_drift() {
        let (provider, reranker) = init_models();
        run_fixture_file(
            &fixtures_dir().join("10_citation_drift.toml"),
            &provider,
            &reranker,
        );
    }

    #[test]
    #[ignore]
    fn drift_fixture_all() {
        let (provider, reranker) = init_models();
        let dir = fixtures_dir();
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("fixtures dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "toml")
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();
        files.sort();
        for path in &files {
            eprintln!("\n=== {} ===", path.file_name().unwrap().to_str().unwrap());
            run_fixture_file(path, &provider, &reranker);
        }
    }
}
