//! Interpretation and task-derivation pipeline for Slack discussion.

use std::{collections::HashMap, sync::OnceLock};

use anyhow::{Result, anyhow};
use regex::Regex;

use crate::{
    llm_extract::LlmTaskExtractor,
    model::{
        ClarificationPrompt, FollowUpItem, FollowUpKind, InvestigationPrompt,
        InvestigationRunCondition, InvestigationTask, ProjectContext, ProjectInterpretation,
        QuestionItem, SlackMessage, TaskBatch, TaskConfidence, TaskSourceKind, WorkflowSeed,
        unix_now,
    },
};

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn candidate_split_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r";|\.\s+").expect("compile candidate split regex"))
}

fn actionable_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(todo|action item|need to|needs to|please|must|should|follow up|ship|deliver|send|update|review|investigate|validate|check)\b",
        )
        .expect("compile actionable regex")
    })
}

fn task_checkbox_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^-?\s*\[[xX ]\]").expect("compile checkbox regex"))
}

fn action_lead_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(todo|action item|action)\s*[:\-]\s*")
            .expect("compile action prefix regex")
    })
}

fn split_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        for fragment in candidate_split_regex()
            .split(line)
            .map(str::trim)
            .filter(|fragment| !fragment.is_empty())
        {
            out.push(fragment.to_string());
        }
    }
    out
}

fn is_actionable_candidate(text: &str) -> bool {
    if text.len() < 6 {
        return false;
    }
    task_checkbox_regex().is_match(text) || actionable_regex().is_match(text)
}

fn normalize_task_text(text: &str) -> String {
    let trimmed = text.trim_start();
    let after_checkbox = task_checkbox_regex().replace(trimmed, "").to_string();
    let cleaned = if after_checkbox.len() < trimmed.len() {
        after_checkbox
    } else {
        trimmed.to_string()
    };
    let cleaned = action_lead_regex().replace(&cleaned, "").to_string();
    collapse_whitespace(&cleaned)
}

fn is_ignored_slack_subtype(subtype: Option<&str>) -> bool {
    matches!(
        subtype.map(str::trim),
        Some(
            "bot_message" | "channel_join" | "channel_leave" | "channel_topic" | "channel_purpose"
        )
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Interpretation backend selection.
pub enum ExtractionMode {
    /// Deterministic regex-based fallback.
    Heuristic,
    /// LLM-backed interpretation (default for CLI).
    Llm,
}

#[derive(Debug, Clone)]
/// Produces project interpretation and derived tasks from polled source messages.
pub struct TaskExtractor {
    max_candidates: usize,
    mode: ExtractionMode,
    llm_extractor: Option<LlmTaskExtractor>,
}

impl Default for TaskExtractor {
    fn default() -> Self {
        Self::new(20)
    }
}

impl TaskExtractor {
    /// Creates an extractor in heuristic mode.
    pub fn new(max_candidates: usize) -> Self {
        Self {
            max_candidates: max_candidates.max(1),
            mode: ExtractionMode::Heuristic,
            llm_extractor: None,
        }
    }

    /// Creates an extractor with an explicit mode.
    ///
    /// In LLM mode this validates provider configuration from environment.
    pub fn with_mode(max_candidates: usize, mode: ExtractionMode) -> Result<Self> {
        let llm_extractor = match mode {
            ExtractionMode::Heuristic => None,
            ExtractionMode::Llm => Some(LlmTaskExtractor::from_env_required()?),
        };

        Ok(Self {
            max_candidates: max_candidates.max(1),
            mode,
            llm_extractor,
        })
    }

    /// Interprets a Slack message window into a [`TaskBatch`].
    pub async fn extract_slack_runtime(
        &self,
        source_label: &str,
        project: &ProjectContext,
        messages: &[SlackMessage],
    ) -> Result<TaskBatch> {
        let interpretation = match self.mode {
            ExtractionMode::Heuristic => self.interpret_slack_heuristic(project, messages),
            ExtractionMode::Llm => {
                let llm_extractor = self.llm_extractor.as_ref().ok_or_else(|| {
                    anyhow!("LLM extraction mode selected but no extractor is configured")
                })?;
                llm_extractor
                    .extract(source_label, project, messages, self.max_candidates)
                    .await?
            }
        };

        let mut derived_tasks = self.derive_tasks_from_interpretation(&interpretation, project);
        derived_tasks.truncate(self.max_candidates);

        Ok(TaskBatch {
            source: TaskSourceKind::Slack,
            source_label: source_label.to_string(),
            generated_at_unix: unix_now(),
            messages_scanned: messages.len(),
            project: project.clone(),
            interpretation,
            derived_tasks,
        })
    }

    fn interpret_slack_heuristic(
        &self,
        project: &ProjectContext,
        messages: &[SlackMessage],
    ) -> ProjectInterpretation {
        let mut prompts: Vec<InvestigationPrompt> = Vec::new();
        let mut questions: Vec<QuestionItem> = Vec::new();

        for message in messages {
            if is_ignored_slack_subtype(message.subtype.as_deref()) {
                continue;
            }
            let text = message.text.trim();
            if text.is_empty() {
                continue;
            }

            if text.contains('?') {
                let question = collapse_whitespace(text);
                if !question.is_empty() {
                    questions.push(QuestionItem {
                        question,
                        blocking: false,
                        suggested_owner: None,
                        sources: vec![message.source.clone()],
                    });
                }
            }

            for candidate in split_candidates(text) {
                if !is_actionable_candidate(&candidate) {
                    continue;
                }

                let normalized = normalize_task_text(&candidate);
                if normalized.is_empty() {
                    continue;
                }

                let key = stable_key("prompt", &normalized, None);
                prompts.push(InvestigationPrompt {
                    key,
                    title: normalized.clone(),
                    goal: if project.repo_available {
                        format!("Investigate implementation impact of: {normalized}")
                    } else {
                        format!("Clarify and prepare follow-up plan for: {normalized}")
                    },
                    prompt: format!(
                        "Investigate this project discussion item in codebase context and report findings with evidence: {normalized}"
                    ),
                    when_to_run: if project.repo_available {
                        InvestigationRunCondition::RepoAvailable
                    } else {
                        InvestigationRunCondition::RepoUnavailable
                    },
                    depends_on: Vec::new(),
                    suggested_steps: Vec::new(),
                    search_queries: vec![normalized.clone()],
                    expected_artifacts: Vec::new(),
                    confidence: TaskConfidence::Medium,
                    sources: vec![message.source.clone()],
                });
            }
        }

        let follow_ups = if project.repo_available {
            Vec::new()
        } else {
            questions
                .iter()
                .take(3)
                .map(|question| FollowUpItem {
                    kind: FollowUpKind::StakeholderQuestion,
                    prompt: format!("Ask project stakeholders to resolve: {}", question.question),
                    urgency: TaskConfidence::Medium,
                    sources: question.sources.clone(),
                })
                .collect()
        };

        let current_objectives = prompts
            .iter()
            .map(|prompt| prompt.title.clone())
            .take(5)
            .collect();
        let investigation_nodes = dedupe_prompts(prompts);
        let clarification_nodes = questions
            .iter()
            .take(2)
            .map(|question| ClarificationPrompt {
                key: stable_key("clarify", &question.question, None),
                question: question.question.clone(),
                blocking: question.blocking,
                suggested_owner: question.suggested_owner.clone(),
                depends_on: Vec::new(),
                sources: question.sources.clone(),
            })
            .collect();

        ProjectInterpretation {
            executive_summary: format!(
                "Heuristic interpretation generated from {} recent Slack messages",
                messages.len()
            ),
            current_objectives,
            decisions_made: Vec::new(),
            open_questions: questions,
            risks: Vec::new(),
            workflow_seed: WorkflowSeed {
                goal: "Investigate and clarify the latest project-channel discussion".to_string(),
                investigation_nodes,
                clarification_nodes,
                follow_up_nodes: follow_ups.clone(),
            },
            follow_ups,
        }
    }

    fn derive_tasks_from_interpretation(
        &self,
        interpretation: &ProjectInterpretation,
        project: &ProjectContext,
    ) -> Vec<InvestigationTask> {
        let mut tasks: Vec<InvestigationTask> = Vec::new();

        for prompt in &interpretation.workflow_seed.investigation_nodes {
            if !prompt_should_run(prompt.when_to_run, project.repo_available) {
                continue;
            }
            let mut description = vec![format!("Goal: {}", prompt.goal)];
            description.push(format!("Agent prompt: {}", prompt.prompt));
            if !prompt.depends_on.is_empty() {
                description.push(format!("Depends on: {}", prompt.depends_on.join(", ")));
            }
            if !prompt.suggested_steps.is_empty() {
                description.push(format!("Steps: {}", prompt.suggested_steps.join(" | ")));
            }
            if !prompt.search_queries.is_empty() {
                description.push(format!(
                    "Search queries: {}",
                    prompt.search_queries.join(", ")
                ));
            }
            if !prompt.expected_artifacts.is_empty() {
                description.push(format!(
                    "Expected artifacts: {}",
                    prompt.expected_artifacts.join(", ")
                ));
            }

            tasks.push(InvestigationTask {
                key: prompt.key.clone(),
                title: prompt.title.clone(),
                description: description.join("\n"),
                priority: prompt.confidence,
                sources: prompt.sources.clone(),
            });
        }

        for follow_up in &interpretation.workflow_seed.follow_up_nodes {
            let key = stable_key("follow-up", &follow_up.prompt, None);
            tasks.push(InvestigationTask {
                key,
                title: "Stakeholder follow-up".to_string(),
                description: follow_up.prompt.clone(),
                priority: follow_up.urgency,
                sources: follow_up.sources.clone(),
            });
        }

        for clarification in &interpretation.workflow_seed.clarification_nodes {
            if !clarification.blocking {
                continue;
            }
            tasks.push(InvestigationTask {
                key: clarification.key.clone(),
                title: "Blocking clarification".to_string(),
                description: clarification.question.clone(),
                priority: TaskConfidence::High,
                sources: clarification.sources.clone(),
            });
        }

        let mut by_key: HashMap<String, InvestigationTask> = HashMap::new();
        for task in tasks {
            by_key.entry(task.key.clone()).or_insert(task);
        }

        let mut out: Vec<InvestigationTask> = by_key.into_values().collect();
        out.sort_by(|left, right| {
            right
                .priority
                .rank()
                .cmp(&left.priority.rank())
                .then_with(|| {
                    left.title
                        .to_ascii_lowercase()
                        .cmp(&right.title.to_ascii_lowercase())
                })
        });
        out
    }
}

fn prompt_should_run(condition: InvestigationRunCondition, repo_available: bool) -> bool {
    match condition {
        InvestigationRunCondition::Always => true,
        InvestigationRunCondition::RepoAvailable => repo_available,
        InvestigationRunCondition::RepoUnavailable => !repo_available,
    }
}

fn dedupe_prompts(prompts: Vec<InvestigationPrompt>) -> Vec<InvestigationPrompt> {
    let mut by_key: HashMap<String, InvestigationPrompt> = HashMap::new();
    for prompt in prompts {
        by_key.entry(prompt.key.clone()).or_insert(prompt);
    }
    by_key.into_values().collect()
}

fn stable_key(prefix: &str, value: &str, secondary: Option<&str>) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    fn fnv1a_extend(mut hash: u64, bytes: &[u8]) -> u64 {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let normalized_primary = value.to_ascii_lowercase();
    let digest = match secondary {
        Some(secondary) => fnv1a_extend(
            fnv1a_extend(
                fnv1a_extend(FNV_OFFSET_BASIS, normalized_primary.as_bytes()),
                &[0x1f],
            ),
            secondary.to_ascii_lowercase().as_bytes(),
        ),
        None => fnv1a_extend(FNV_OFFSET_BASIS, normalized_primary.as_bytes()),
    };

    format!("{prefix}-{digest:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceReference;

    fn message(text: &str, ts: &str) -> SlackMessage {
        SlackMessage {
            channel_name: "agentium-eng".to_string(),
            channel_id: "C12345".to_string(),
            ts: ts.to_string(),
            thread_ts: None,
            user_id: Some("U1".to_string()),
            user_name: Some("alice".to_string()),
            text: text.to_string(),
            subtype: None,
            source: SourceReference {
                reference: format!("slack://channel/C12345/p{}", ts.replace('.', "")),
                permalink: None,
                channel_id: Some("C12345".to_string()),
                message_ts: Some(ts.to_string()),
                thread_ts: None,
            },
        }
    }

    #[tokio::test]
    async fn heuristic_mode_derives_tasks_from_interpretation() {
        let extractor = TaskExtractor::with_mode(20, ExtractionMode::Heuristic).expect("extractor");
        let batch = extractor
            .extract_slack_runtime(
                "#agentium-eng",
                &ProjectContext {
                    project_key: "agent-platform".to_string(),
                    repo_available: true,
                    repo_path: Some("/repo".to_string()),
                },
                &[
                    message(
                        "TODO: investigate daemon cursor behavior",
                        "1735689600.000000",
                    ),
                    message("Should we support at-least-once?", "1735689700.000000"),
                ],
            )
            .await
            .expect("extract");

        assert!(
            !batch
                .interpretation
                .workflow_seed
                .investigation_nodes
                .is_empty()
        );
        assert!(!batch.derived_tasks.is_empty());
    }

    #[test]
    fn prompt_run_condition_honored() {
        assert!(prompt_should_run(InvestigationRunCondition::Always, false));
        assert!(prompt_should_run(
            InvestigationRunCondition::RepoAvailable,
            true
        ));
        assert!(!prompt_should_run(
            InvestigationRunCondition::RepoUnavailable,
            true
        ));
    }
}
