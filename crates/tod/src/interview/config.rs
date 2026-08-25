use crate::interview::db::InterviewSession;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterviewConfig {
    pub session_id: String,
    pub entity: PathBuf,
    pub phase: String,
    pub transcript: PathBuf,
    pub scratchpad: PathBuf,
    pub queue: PathBuf,
    pub config_path: PathBuf,
    pub queue_target: Option<u32>,
    pub to_process: Option<PathBuf>,
    pub researcher_status: Option<PathBuf>,
    pub answer_processor_status: Option<PathBuf>,
    pub scope: Vec<PathBuf>,
    pub state_agent: Option<PathBuf>,
}

pub fn parse_interview_config(path: &Path) -> Result<InterviewConfig> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read interview config {}", path.display()))?;
    parse_interview_config_contents(path, &contents)
}

pub fn parse_interview_config_contents(path: &Path, contents: &str) -> Result<InterviewConfig> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut scope: Vec<PathBuf> = Vec::new();
    let mut in_scope = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "scope:" {
            in_scope = true;
            continue;
        }
        if in_scope {
            if trimmed.starts_with("- ") {
                scope.push(PathBuf::from(trimmed.trim_start_matches("- ").trim()));
                continue;
            }
            in_scope = false;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let session_id = values
        .get("session_id")
        .cloned()
        .context("interview-config missing session_id")?;
    let entity = values
        .get("entity")
        .map(PathBuf::from)
        .context("interview-config missing entity")?;
    let phase = values
        .get("phase")
        .cloned()
        .context("interview-config missing phase")?;
    let transcript = values
        .get("transcript")
        .map(PathBuf::from)
        .context("interview-config missing transcript")?;
    let scratchpad = values
        .get("scratchpad")
        .map(PathBuf::from)
        .context("interview-config missing scratchpad")?;
    let queue = values
        .get("queue")
        .map(|q| PathBuf::from(q.trim_end_matches('/')))
        .context("interview-config missing queue")?;
    let queue_target = values
        .get("queue_target")
        .and_then(|v| v.parse().ok());

    Ok(InterviewConfig {
        session_id,
        entity,
        phase,
        transcript,
        scratchpad,
        queue,
        config_path: path.to_path_buf(),
        queue_target,
        to_process: values.get("to_process").map(PathBuf::from),
        researcher_status: values.get("researcher_status").map(PathBuf::from),
        answer_processor_status: values
            .get("answer_processor_status")
            .map(PathBuf::from),
        scope,
        state_agent: values.get("state_agent").map(PathBuf::from),
    })
}

pub fn agent_scratchpad_for_entity(repo_root: &Path, entity: &Path) -> Result<PathBuf> {
    let entity = entity.canonicalize().unwrap_or_else(|_| entity.to_path_buf());
    let repo_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
    let entity_str = entity.to_string_lossy();
    let repo_str = repo_root.to_string_lossy();
    if !entity_str.starts_with(repo_str.as_ref()) {
        bail!("entity path must be inside repo root for scratchpad mirroring");
    }
    let rel = entity.strip_prefix(&repo_root).unwrap_or(entity.as_path());
    Ok(repo_root
        .join(".local")
        .join("agent")
        .join("process")
        .join(rel)
        .join("scratchpad")
        .join("interviews"))
}

/// Base phase token from SQLite metadata (strips optional parenthetical note).
pub fn base_interview_phase(phase: &str) -> &str {
    phase.split('(').next().unwrap_or(phase).trim()
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
}

fn paths_match(a: &Path, b: &Path) -> bool {
    normalize_path(a) == normalize_path(b)
}

fn system_time_to_utc(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

/// List `interview-config.md` files under `root` (recursive).
pub fn list_interview_config_paths(root: &Path) -> Vec<PathBuf> {
    let mut configs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("interview-config.md") {
                configs.push(path);
            }
        }
    }
    configs
}

/// Find the interview-config created for a UI kickoff session.
///
/// Matches entity + base phase and picks the config whose mtime is earliest after
/// kickoff (not the globally newest config on disk).
pub fn find_bootstrap_config_for_session(
    repo_root: &Path,
    session: &InterviewSession,
) -> Result<Option<PathBuf>> {
    if let Some(ref path) = session.config_path {
        let path = Path::new(path);
        if path.exists() {
            return Ok(Some(path.to_path_buf()));
        }
    }

    let entity_path = match session.entity_path.as_ref().filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => return Ok(None),
    };
    let base_phase = session
        .phase
        .as_deref()
        .map(base_interview_phase)
        .unwrap_or_default();
    let kickoff = session.created_at;

    let mut search_roots = Vec::new();
    if let Ok(scratchpad_root) = agent_scratchpad_for_entity(repo_root, &entity_path) {
        search_roots.push(scratchpad_root);
    }
    search_roots.push(
        repo_root
            .join(".local")
            .join("agent")
            .join("process"),
    );

    let mut best: Option<(i64, PathBuf)> = None;
    let mut seen = HashMap::new();
    for root in search_roots {
        for config_path in list_interview_config_paths(&root) {
            if seen.insert(config_path.clone(), ()).is_some() {
                continue;
            }
            let Ok(config) = parse_interview_config(&config_path) else {
                continue;
            };
            if !paths_match(&config.entity, &entity_path) {
                continue;
            }
            if !base_phase.is_empty() && config.phase != base_phase {
                continue;
            }
            let Ok(modified) = config_path.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let modified_utc = system_time_to_utc(modified);
            let delta_ms = (modified_utc - kickoff).num_milliseconds();
            if delta_ms < -5_000 {
                continue;
            }
            match &best {
                None => best = Some((delta_ms, config_path)),
                Some((best_delta, _)) if delta_ms < *best_delta => {
                    best = Some((delta_ms, config_path));
                }
                _ => {}
            }
        }
    }

    Ok(best.map(|(_, path)| path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_config() {
        let text = r#"# Interview config

session_id: design-interview-2026-08-23-1330
entity: c:\data\git\tod\doc\process\projects\interview-ui\tasks\core-ui
phase: design-interview
transcript: c:\data\git\tod\doc\process\projects\interview-ui\tasks\core-ui\history\design-interview-2026-08-23-1330.md
scope:
  - c:\data\git\tod\doc\process\projects\interview-ui\tasks\core-ui\user.md
state_agent: C:\Users\joel\.claude\skills\process\agents\design-agent.md
scratchpad: c:\data\git\tod\.local\agent\process\projects\interview-ui\tasks\core-ui\scratchpad\interviews\design-interview-2026-08-23-1330
queue: c:\data\git\tod\.local\agent\process\projects\interview-ui\tasks\core-ui\scratchpad\interviews\design-interview-2026-08-23-1330\queue/
queue_target: 8
"#;
        let cfg = parse_interview_config_contents(Path::new("interview-config.md"), text).unwrap();
        assert_eq!(cfg.session_id, "design-interview-2026-08-23-1330");
        assert_eq!(cfg.scope.len(), 1);
        assert_eq!(cfg.queue_target, Some(8));
    }

    #[test]
    fn base_phase_strips_parenthetical_note() {
        assert_eq!(
            base_interview_phase("design-interview (Optional note)"),
            "design-interview"
        );
    }

    #[test]
    fn find_bootstrap_config_matches_entity_and_phase_not_newest_global() {
        use crate::interview::db::{InterviewSession, InterviewSessionStatus};
        use chrono::TimeZone;
        use std::fs;

        let dir = std::env::temp_dir().join(format!("tod-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.as_path();
        let entity = repo.join("doc").join("entity-a");
        let entity_b = repo.join("doc").join("entity-b");
        std::fs::create_dir_all(&entity).unwrap();
        std::fs::create_dir_all(&entity_b).unwrap();

        let old_config_dir = agent_scratchpad_for_entity(repo, &entity_b).unwrap().join("old-session");
        std::fs::create_dir_all(&old_config_dir).unwrap();
        let old_config = old_config_dir.join("interview-config.md");
        std::fs::write(
            &old_config,
            format!(
                "session_id: old-session\nentity: {}\nphase: design-interview\ntranscript: {}/t.md\nscratchpad: {}\nqueue: {}/queue/\n",
                entity_b.display(),
                old_config_dir.display(),
                old_config_dir.display(),
                old_config_dir.display(),
            ),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let kickoff = Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0).unwrap();
        let new_config_dir =
            agent_scratchpad_for_entity(repo, &entity).unwrap().join("new-session");
        std::fs::create_dir_all(&new_config_dir).unwrap();
        let new_config = new_config_dir.join("interview-config.md");
        std::fs::write(
            &new_config,
            format!(
                "session_id: new-session\nentity: {}\nphase: planning-interview\ntranscript: {}/t.md\nscratchpad: {}\nqueue: {}/queue/\n",
                entity.display(),
                new_config_dir.display(),
                new_config_dir.display(),
                new_config_dir.display(),
            ),
        )
        .unwrap();

        let session = InterviewSession {
            id: 1,
            display_name: "Entity A — Planning".into(),
            status: InterviewSessionStatus::Active,
            entity_path: Some(entity.to_string_lossy().into()),
            phase: Some("planning-interview".into()),
            session_id: None,
            scratchpad_path: None,
            transcript_path: None,
            config_path: None,
            created_at: kickoff,
            updated_at: kickoff,
        };

        let found = find_bootstrap_config_for_session(repo, &session)
            .unwrap()
            .expect("expected matching config");
        assert_eq!(found, new_config);
    }
}
