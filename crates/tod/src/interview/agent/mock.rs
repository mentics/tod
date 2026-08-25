use super::provider::{
    AgentProvider, AgentRunHandle, AgentRunKind, AgentRunState, DeepDiveContext, RunId,
};
use crate::interview::config::agent_scratchpad_for_entity;
use crate::interview::transcript::new_transcript_filename;
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Fast in-process agent backend for UI tests. Writes realistic on-disk scaffolding
/// and queue/status files; never calls Cursor ACP or any external process.
pub struct MockAgentProvider {
    runs: HashMap<RunId, AgentRunState>,
}

impl MockAgentProvider {
    pub fn new() -> Self {
        Self {
            runs: HashMap::new(),
        }
    }

    fn finish(&mut self, kind: AgentRunKind, state: AgentRunState) -> AgentRunHandle {
        let id = RunId::new();
        self.runs.insert(id, state.clone());
        AgentRunHandle { id, kind, state }
    }
}

impl Default for MockAgentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for MockAgentProvider {
    fn start_researcher_replenishment(
        &mut self,
        _cwd: PathBuf,
        prompt: String,
    ) -> Result<AgentRunHandle> {
        let msg = if prompt.contains("Interview UI kickoff") {
            bootstrap_from_prompt(&prompt)?
        } else if prompt.contains("Action payload")
            || prompt.contains("Process researcher action")
        {
            action_from_prompt(&prompt)?
        } else {
            replenish_from_prompt(&prompt)?
        };
        Ok(self.finish(
            AgentRunKind::ResearcherReplenishment,
            AgentRunState::Success(Some(msg)),
        ))
    }

    fn start_answer_processor(&mut self, _cwd: PathBuf, prompt: String) -> Result<AgentRunHandle> {
        let msg = process_answer_from_prompt(&prompt)?;
        Ok(self.finish(
            AgentRunKind::AnswerProcessor,
            AgentRunState::Success(Some(msg)),
        ))
    }

    fn start_deep_dive_chat(
        &mut self,
        _cwd: PathBuf,
        context: DeepDiveContext,
        initial_message: Option<String>,
    ) -> Result<AgentRunHandle> {
        let body = initial_message.unwrap_or_else(|| context.question_body.clone());
        let reply = format!(
            "Mock deep-dive reply for {}:\n\nConsider: {}\n\n(Use this text is available.)",
            context.question_id,
            body.chars().take(240).collect::<String>()
        );
        Ok(self.finish(
            AgentRunKind::DeepDiveChat,
            AgentRunState::Success(Some(reply)),
        ))
    }

    fn poll_run(&mut self, id: RunId) -> Option<AgentRunState> {
        self.runs.get(&id).cloned()
    }

    fn cancel_run(&mut self, id: RunId) -> Result<()> {
        self.runs.remove(&id);
        Ok(())
    }
}

fn bootstrap_from_prompt(prompt: &str) -> Result<String> {
    let entity =
        prompt_field(prompt, "Entity path").context("mock bootstrap: missing Entity path")?;
    let phase = prompt_field(prompt, "Phase/purpose").unwrap_or_else(|| "project-defining".into());
    let phase = phase.split('(').next().unwrap_or(&phase).trim().to_string();
    let entity_path = PathBuf::from(&entity);
    let repo_root = infer_repo_root(&entity_path)?;
    let now = Local::now();
    let session_stem =
        new_transcript_filename(&format!("{}-interview", phase.replace(' ', "-")), now)
            .trim_end_matches(".md")
            .to_string();

    let history_dir = entity_path.join("history");
    fs::create_dir_all(&history_dir)
        .with_context(|| format!("create history {}", history_dir.display()))?;
    let transcript = history_dir.join(format!("{session_stem}.md"));
    fs::write(
        &transcript,
        format!(
            "# Mock interview — {session_stem}\n\n## Session\n\n**Entity:** {}\n**Phase:** {phase}\n\n",
            entity_path.display()
        ),
    )?;

    // agent_scratchpad_for_entity already ends at …/scratchpad/interviews
    let scratch = agent_scratchpad_for_entity(&repo_root, &entity_path)?.join(&session_stem);
    let queue = scratch.join("queue");
    fs::create_dir_all(&queue)?;

    let config_path = scratch.join("interview-config.md");
    let researcher_status = scratch.join("researcher-status.md");
    let answer_processor_status = scratch.join("answer-processor-status.md");
    let to_process = agent_scratchpad_for_entity(&repo_root, &entity_path)?
        .parent() // …/scratchpad
        .unwrap_or(scratch.as_path())
        .join("to-process.md");

    let config = format!(
        "# Interview config\n\n\
session_id: {session_stem}\n\
entity: {}\n\
phase: {phase}\n\
transcript: {}\n\
scope:\n\
  - {}\n\
scratchpad: {}\n\
queue: {}\n\
queue_target: 8\n\
to_process: {}\n\
researcher_status: {}\n\
answer_processor_status: {}\n",
        entity_path.display(),
        transcript.display(),
        entity_path.join("user.md").display(),
        scratch.display(),
        queue.display(),
        to_process.display(),
        researcher_status.display(),
        answer_processor_status.display(),
    );
    fs::write(&config_path, config)?;
    write_status(&researcher_status, "idle", "mock bootstrap complete")?;
    write_status(&answer_processor_status, "idle", "")?;
    write_queue_questions(&queue, 8, "mock-bootstrap")?;

    Ok(queue.display().to_string())
}

fn replenish_from_prompt(prompt: &str) -> Result<String> {
    let config_path =
        prompt_field(prompt, "Config path").context("mock replenish: missing Config path")?;
    let target: u32 = prompt
        .lines()
        .find_map(|l| l.strip_prefix("Target open question count: "))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(8);

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "researcher_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let existing = count_queue_files(&queue_dir);
    // Empty queue on replenish = no further questions (interview-complete signal).
    if existing == 0 {
        if let Some(status_path) = &status {
            write_status(
                &PathBuf::from(status_path),
                "complete",
                "no further questions",
            )?;
        }
        return Ok(queue_dir.display().to_string());
    }

    let need = target.saturating_sub(existing);
    if need > 0 {
        let start = existing + 1;
        for i in 0..need {
            let n = start + i;
            write_one_question(&queue_dir, n, "mock-replenish")?;
        }
    }

    if let Some(status_path) = status {
        let open = count_queue_files(&queue_dir);
        if open == 0 {
            write_status(
                &PathBuf::from(status_path),
                "complete",
                "no further questions",
            )?;
        } else {
            write_status(
                &PathBuf::from(status_path),
                "idle",
                "mock replenish complete",
            )?;
        }
    }

    Ok(queue_dir.display().to_string())
}

fn action_from_prompt(prompt: &str) -> Result<String> {
    let config_path =
        prompt_field(prompt, "Config path").context("mock action: missing Config path")?;
    let actions = extract_actions(prompt);
    if actions.is_empty() {
        bail!("mock action: no action/id in payload");
    }

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "researcher_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let mut handled = Vec::new();
    for (action, id) in &actions {
        match action.as_str() {
            "defer" => {
                if delete_queue_question(&queue_dir, id)? {
                    handled.push(format!("defer:{id}"));
                }
            }
            "reconsider" => {
                if rewrite_queue_question(&queue_dir, id, |body| {
                    format!(
                        "{}\n\n*(Mock reconsider — please re-check this question.)*\n",
                        body.trim_end()
                    )
                })? {
                    handled.push(format!("reconsider:{id}"));
                }
            }
            "more-options" => {
                if add_more_options(&queue_dir, id)? {
                    handled.push(format!("more-options:{id}"));
                }
            }
            other => bail!("mock action: unsupported action {other}"),
        }
    }

    if let Some(status_path) = status {
        write_status(
            &PathBuf::from(status_path),
            "idle",
            &format!("actions: {}", handled.join(",")),
        )?;
    }

    Ok(format!("actions: {}", handled.join(",")))
}

fn process_answer_from_prompt(prompt: &str) -> Result<String> {
    let config_path = prompt_field(prompt, "Config path")
        .context("mock answer-processor: missing Config path")?;
    let ids = extract_answer_ids(prompt);
    if ids.is_empty() {
        bail!("mock answer-processor: no answer id in payload");
    }

    let config_text =
        fs::read_to_string(&config_path).with_context(|| format!("read config {config_path}"))?;
    let queue = config_value(&config_text, "queue").context("config missing queue")?;
    let status = config_value(&config_text, "answer_processor_status");
    let queue_dir = PathBuf::from(queue.trim_end_matches(['/', '\\']));

    let mut resolved = Vec::new();
    for id in &ids {
        if delete_queue_question(&queue_dir, id)? {
            resolved.push(id.clone());
        }
    }

    if let Some(status_path) = status {
        write_status(
            &PathBuf::from(status_path),
            "idle",
            &format!("resolved: {}", resolved.join(",")),
        )?;
    }

    // Last question cleared → signal interview complete for UI (no further questions).
    if count_queue_files(&queue_dir) == 0 {
        if let Some(rs) = config_value(&config_text, "researcher_status") {
            write_status(&PathBuf::from(rs), "complete", "no further questions")?;
        }
    }

    Ok(format!("resolved: {}\nmodified:", resolved.join(",")))
}

fn prompt_field(prompt: &str, label: &str) -> Option<String> {
    let prefix = format!("{label}: ");
    prompt.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

fn config_value(config: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    config.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

fn write_status(path: &Path, status: &str, message: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = if message.is_empty() {
        format!("status: {status}\n")
    } else {
        format!("status: {status}\nmessage: {message}\n")
    };
    fs::write(path, body)?;
    Ok(())
}

fn count_queue_files(queue: &Path) -> u32 {
    fs::read_dir(queue)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| x.eq_ignore_ascii_case("md"))
                })
                .count() as u32
        })
        .unwrap_or(0)
}

fn write_queue_questions(queue: &Path, count: u32, tag: &str) -> Result<()> {
    for n in 1..=count {
        write_one_question(queue, n, tag)?;
    }
    Ok(())
}

fn write_one_question(queue: &Path, n: u32, tag: &str) -> Result<()> {
    let id = format!("q-{n:03}");
    let path = queue.join(format!("{id}-{tag}.md"));
    let body = if n % 2 == 1 {
        format!(
            "---\nid: {id}\ncreated: 2026-08-24T12:00:00Z\noptions:\n  - key: \"1\"\n    label: Option One\n  - key: \"2\"\n    label: Option Two\n---\nMock MC question {n} ({tag})\n"
        )
    } else {
        format!(
            "---\nid: {id}\ncreated: 2026-08-24T12:00:00Z\n---\nApprove this mock statement for question {n}?\n"
        )
    };
    fs::write(path, body)?;
    Ok(())
}

fn find_queue_question_path(queue: &Path, id: &str) -> Option<PathBuf> {
    let Ok(rd) = fs::read_dir(queue) else {
        return None;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l.trim() == format!("id: {id}"))
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{id}-")) || n == &format!("{id}.md"))
        {
            return Some(path);
        }
    }
    None
}

fn rewrite_queue_question(
    queue: &Path,
    id: &str,
    mutate_body: impl FnOnce(&str) -> String,
) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    let text = fs::read_to_string(&path)?;
    let (front, body) = split_front_matter(&text);
    let new_body = mutate_body(body);
    fs::write(path, format!("{front}{new_body}"))?;
    Ok(true)
}

fn add_more_options(queue: &Path, id: &str) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    let text = fs::read_to_string(&path)?;
    let (front, body) = split_front_matter(&text);
    let next_key = next_option_key(front);
    let meta = front
        .trim_end()
        .trim_end_matches("---")
        .trim_end()
        .to_string();
    let mut rebuilt = meta;
    if !rebuilt.contains("options:") {
        rebuilt.push_str("\noptions:");
    }
    rebuilt.push_str(&format!(
        "\n  - key: \"{next_key}\"\n    label: Mock extra option {next_key}\n---\n"
    ));
    fs::write(path, format!("{rebuilt}{body}"))?;
    Ok(true)
}

fn split_front_matter(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some((meta, body)) = rest.split_once("\n---\n") {
            let front_end = "---\n".len() + meta.len() + "\n---\n".len();
            return (&text[..front_end], body);
        }
    }
    (text, "")
}

fn next_option_key(front: &str) -> u32 {
    let mut max = 0u32;
    for line in front.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- key:") {
            let key = rest.trim().trim_matches('"');
            if let Ok(n) = key.parse::<u32>() {
                max = max.max(n);
            }
        }
    }
    max + 1
}

fn delete_queue_question(queue: &Path, id: &str) -> Result<bool> {
    let Some(path) = find_queue_question_path(queue, id) else {
        return Ok(false);
    };
    fs::remove_file(&path)?;
    Ok(true)
}

/// Infer the sandbox/repo root from an entity under `doc/process/...`.
fn infer_repo_root(entity: &Path) -> Result<PathBuf> {
    let raw = entity.to_string_lossy();
    for marker in ["doc\\process", "doc/process"] {
        if let Some(idx) = raw.find(marker) {
            let root = raw[..idx].trim_end_matches(['/', '\\']);
            if !root.is_empty() {
                return Ok(PathBuf::from(root));
            }
        }
    }
    // Fallback: walk parents for `.local` / `.git`.
    let mut dir = entity.to_path_buf();
    loop {
        if dir.join(".local").is_dir() || dir.join(".git").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!(
                "mock bootstrap: cannot infer repo root from entity {}",
                entity.display()
            );
        }
    }
}

fn extract_answer_ids(prompt: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut in_payload = false;
    for line in prompt.lines() {
        if line.contains("Answer payload") || line.contains("Action payload") {
            in_payload = true;
            continue;
        }
        if !in_payload {
            // Also accept bare `id:` anywhere in the prompt.
            if let Some(rest) = line.trim().strip_prefix("id:") {
                let id = rest.trim().trim_matches('"').to_string();
                if !id.is_empty() && !ids.contains(&id) {
                    ids.push(id);
                }
            }
            continue;
        }
        if let Some(rest) = line.trim().strip_prefix("- id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if !id.is_empty() {
                ids.push(id);
            }
        } else if let Some(rest) = line.trim().strip_prefix("id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn extract_actions(prompt: &str) -> Vec<(String, String)> {
    let mut actions = Vec::new();
    let mut pending_action: Option<String> = None;
    let mut in_payload = false;
    for line in prompt.lines() {
        if line.contains("Action payload") {
            in_payload = true;
            continue;
        }
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("action:") {
            pending_action = Some(rest.trim().trim_matches('"').to_string());
            continue;
        }
        if let Some(rest) = t.strip_prefix("id:") {
            let id = rest.trim().trim_matches('"').to_string();
            if let Some(action) = pending_action.take() {
                if !id.is_empty() {
                    actions.push((action, id));
                }
            } else if in_payload && !id.is_empty() {
                // id without preceding action in this unit — skip
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_bootstrap_writes_config_and_queue() {
        let root = std::env::temp_dir().join(format!("tod-mock-{}", uuid::Uuid::new_v4()));
        let entity = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("sandbox")
            .join("tasks")
            .join("smoke");
        fs::create_dir_all(entity.join("history")).unwrap();
        fs::write(entity.join("user.md"), "# Smoke\n").unwrap();

        let mut mock = MockAgentProvider::new();
        let prompt = format!(
            "Interview UI kickoff — bootstrap this interview session.\n\
             SQLite session id: 1\n\
             Display name: smoke — Initial\n\
             Entity path: {}\n\
             Phase/purpose: project-defining\n",
            entity.display()
        );
        let handle = mock
            .start_researcher_replenishment(entity.clone(), prompt)
            .unwrap();
        assert!(matches!(
            mock.poll_run(handle.id),
            Some(AgentRunState::Success(_))
        ));

        let scratch = agent_scratchpad_for_entity(&root, &entity).unwrap();
        let sessions: Vec<_> = fs::read_dir(&scratch)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(sessions.len(), 1);
        let session_dir = sessions[0].path();
        assert!(session_dir.join("interview-config.md").is_file());
        assert_eq!(count_queue_files(&session_dir.join("queue")), 8);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mock_action_defer_deletes_question() {
        let root = std::env::temp_dir().join(format!("tod-mock-act-{}", uuid::Uuid::new_v4()));
        let entity = root
            .join("doc")
            .join("process")
            .join("projects")
            .join("sandbox")
            .join("tasks")
            .join("smoke");
        fs::create_dir_all(entity.join("history")).unwrap();
        fs::write(entity.join("user.md"), "# Smoke\n").unwrap();

        let mut mock = MockAgentProvider::new();
        let prompt = format!(
            "Interview UI kickoff — bootstrap this interview session.\n\
             SQLite session id: 1\n\
             Display name: smoke — Initial\n\
             Entity path: {}\n\
             Phase/purpose: project-defining\n",
            entity.display()
        );
        mock.start_researcher_replenishment(entity.clone(), prompt)
            .unwrap();

        let scratch = agent_scratchpad_for_entity(&root, &entity).unwrap();
        let session_dir = fs::read_dir(&scratch).unwrap().next().unwrap().unwrap().path();
        let config_path = session_dir.join("interview-config.md");
        let queue = session_dir.join("queue");
        assert!(find_queue_question_path(&queue, "q-001").is_some());

        let action_prompt = format!(
            "Process researcher action submission.\n\
             Config path: {}\n\
             Action payload (YAML multi-record):\n\
             ---\naction: defer\nid: q-001\n---\n",
            config_path.display()
        );
        mock.start_researcher_replenishment(entity, action_prompt)
            .unwrap();
        assert!(find_queue_question_path(&queue, "q-001").is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
