use crate::interview::db::{InterviewSession, SessionStore};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
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
                scope.push(PathBuf::from(normalize_config_path(
                    trimmed.trim_start_matches("- ").trim(),
                )));
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
        .map(|v| PathBuf::from(normalize_config_path(v)))
        .context("interview-config missing entity")?;
    let phase = values
        .get("phase")
        .cloned()
        .context("interview-config missing phase")?;
    let transcript = values
        .get("transcript")
        .map(|v| PathBuf::from(normalize_config_path(v)))
        .context("interview-config missing transcript")?;
    let scratchpad = values
        .get("scratchpad")
        .map(|v| PathBuf::from(normalize_config_path(v)))
        .context("interview-config missing scratchpad")?;
    let queue = values
        .get("queue")
        .map(|q| PathBuf::from(normalize_config_path(q.trim_end_matches(['/', '\\']))))
        .context("interview-config missing queue")?;
    let queue_target = values.get("queue_target").and_then(|v| v.parse().ok());

    Ok(InterviewConfig {
        session_id,
        entity,
        phase,
        transcript,
        scratchpad,
        queue,
        config_path: path.to_path_buf(),
        queue_target,
        to_process: values
            .get("to_process")
            .map(|v| PathBuf::from(normalize_config_path(v))),
        researcher_status: values
            .get("researcher_status")
            .map(|v| PathBuf::from(normalize_config_path(v))),
        answer_processor_status: values
            .get("answer_processor_status")
            .map(|v| PathBuf::from(normalize_config_path(v))),
        scope,
        state_agent: values
            .get("state_agent")
            .map(|v| PathBuf::from(normalize_config_path(v))),
    })
}

/// Strip Windows verbatim (`\\?\`) prefixes so queue watchers / exists checks work.
fn normalize_config_path(raw: &str) -> String {
    let trimmed = raw.trim();
    #[cfg(windows)]
    {
        if let Some(stripped) = trimmed.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    trimmed.to_string()
}

pub fn agent_scratchpad_for_entity(repo_root: &Path, entity: &Path) -> Result<PathBuf> {
    let entity = entity
        .canonicalize()
        .unwrap_or_else(|_| entity.to_path_buf());
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let rel = entity.strip_prefix(&repo_root).map_err(|_| {
        anyhow::anyhow!("entity path must be inside repo root for scratchpad mirroring")
    })?;
    // Live entities live under doc/process/; ephemeral mirror drops that prefix:
    // doc/process/projects/X → .local/agent/process/projects/X
    let mirrored = strip_doc_process_prefix(rel);
    Ok(repo_root
        .join(".local")
        .join("agent")
        .join("process")
        .join(mirrored)
        .join("scratchpad")
        .join("interviews"))
}

/// Strip leading `doc/process` from a repo-relative entity path.
pub fn strip_doc_process_prefix(rel: &Path) -> PathBuf {
    let mut comps = rel.components();
    let first = comps.next();
    let second = comps.next();
    match (first, second) {
        (Some(std::path::Component::Normal(a)), Some(std::path::Component::Normal(b)))
            if a == "doc" && b == "process" =>
        {
            comps.as_path().to_path_buf()
        }
        _ => rel.to_path_buf(),
    }
}

/// Base phase token from SQLite metadata (strips optional parenthetical note).
pub fn base_interview_phase(phase: &str) -> &str {
    phase.split('(').next().unwrap_or(phase).trim()
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Strip Windows verbatim (`\\?\`) prefix for durable path storage / UI.
pub fn path_for_storage(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    raw.into_owned()
}

pub(crate) fn paths_match(a: &Path, b: &Path) -> bool {
    let na = normalize_path(a);
    let nb = normalize_path(b);
    if na == nb {
        return true;
    }
    // Windows paths often differ only by drive-letter case when canonicalize fails.
    na.to_string_lossy()
        .eq_ignore_ascii_case(nb.to_string_lossy().as_ref())
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
    search_roots.push(repo_root.join(".local").join("agent").join("process"));

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

/// Bind SQLite session scaffolding paths from a matching on-disk `interview-config.md`.
///
/// Returns `Ok(true)` when paths are already set or were just updated; `Ok(false)` when
/// no matching config is visible yet (caller should retry).
pub fn sync_scaffolding_from_disk(
    store: &SessionStore,
    repo_root: &Path,
    sqlite_id: i64,
) -> Result<bool> {
    let session = store
        .get_session(sqlite_id)?
        .ok_or_else(|| anyhow::anyhow!("session {sqlite_id} not found"))?;
    if session
        .config_path
        .as_ref()
        .is_some_and(|p| Path::new(p).exists())
    {
        return Ok(true);
    }

    let claimed: HashSet<String> = store
        .list_sessions()?
        .into_iter()
        .filter(|s| s.id != sqlite_id)
        .filter_map(|s| s.config_path)
        .map(|p| {
            let pb = PathBuf::from(&p);
            pb.canonicalize()
                .unwrap_or(pb)
                .to_string_lossy()
                .to_ascii_lowercase()
        })
        .collect();

    let Some(config_path) = find_bootstrap_config_for_session(repo_root, &session)? else {
        tracing::debug!(
            event = "interview",
            action = "sync_no_config",
            sqlite_id,
            entity = session.entity_path.as_deref().unwrap_or(""),
            phase = session.phase.as_deref().unwrap_or(""),
            "find_bootstrap_config_for_session returned None"
        );
        return Ok(false);
    };
    let config = parse_interview_config(&config_path)?;
    // Hard guarantee: never bind another entity's queue to this session.
    if let Some(entity) = session.entity_path.as_ref() {
        if !paths_match(&config.entity, Path::new(entity)) {
            tracing::warn!(
                event = "interview",
                action = "sync_entity_mismatch",
                sqlite_id,
                session_entity = %entity,
                config_entity = %config.entity.display(),
                config = %config_path.display(),
                "refusing to bind interview-config for a different entity"
            );
            return Ok(false);
        }
    }
    let canon = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.clone())
        .to_string_lossy()
        .to_ascii_lowercase();
    if claimed.contains(&canon) {
        // Another session already owns this config — wait for a fresh bootstrap write.
        tracing::warn!(
            event = "interview",
            action = "sync_config_claimed",
            sqlite_id,
            config = %config_path.display(),
            "matching config already claimed by another session"
        );
        return Ok(false);
    }
    if !scaffolding_ready_to_bind(&config) {
        tracing::debug!(
            event = "interview",
            action = "sync_wait_queue",
            sqlite_id,
            config = %config_path.display(),
            "config present but queue not ready yet"
        );
        return Ok(false);
    }
    bind_session_scaffolding(store, sqlite_id, &config_path, &config)
}

/// Like [`sync_scaffolding_from_disk`], but binds even when the queue is still empty.
/// Used after the bootstrap ACP run exits so a deliberate empty interview can open.
pub fn sync_scaffolding_from_disk_after_bootstrap(
    store: &SessionStore,
    repo_root: &Path,
    sqlite_id: i64,
) -> Result<bool> {
    let session = store
        .get_session(sqlite_id)?
        .ok_or_else(|| anyhow::anyhow!("session {sqlite_id} not found"))?;
    if session
        .config_path
        .as_ref()
        .is_some_and(|p| Path::new(p).exists())
    {
        return Ok(true);
    }

    let claimed: HashSet<String> = store
        .list_sessions()?
        .into_iter()
        .filter(|s| s.id != sqlite_id)
        .filter_map(|s| s.config_path)
        .map(|p| {
            let pb = PathBuf::from(&p);
            pb.canonicalize()
                .unwrap_or(pb)
                .to_string_lossy()
                .to_ascii_lowercase()
        })
        .collect();

    let Some(config_path) = find_bootstrap_config_for_session(repo_root, &session)? else {
        return Ok(false);
    };
    let config = parse_interview_config(&config_path)?;
    if let Some(entity) = session.entity_path.as_ref() {
        if !paths_match(&config.entity, Path::new(entity)) {
            tracing::warn!(
                event = "interview",
                action = "sync_entity_mismatch",
                sqlite_id,
                session_entity = %entity,
                config_entity = %config.entity.display(),
                "refusing to bind interview-config for a different entity"
            );
            return Ok(false);
        }
    }
    let canon = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.clone())
        .to_string_lossy()
        .to_ascii_lowercase();
    if claimed.contains(&canon) {
        return Ok(false);
    }
    bind_session_scaffolding(store, sqlite_id, &config_path, &config)
}

fn bind_session_scaffolding(
    store: &SessionStore,
    sqlite_id: i64,
    config_path: &Path,
    config: &InterviewConfig,
) -> Result<bool> {
    tracing::info!(
        event = "interview",
        action = "sync_bind",
        sqlite_id,
        config = %config_path.display(),
        session_stem = %config.session_id,
        queue = %config.queue.display(),
        "binding scaffolding paths from disk"
    );
    store.update_session_scaffolding(
        sqlite_id,
        Some(&config.session_id),
        Some(&path_for_storage(&config.scratchpad)),
        Some(&path_for_storage(&config.transcript)),
        Some(&path_for_storage(&config.config_path)),
    )?;
    Ok(true)
}

/// Bind only once open question files exist. Empty-queue bind is deferred until
/// bootstrap ACP finishes ([`sync_scaffolding_from_disk_after_bootstrap`]).
fn scaffolding_ready_to_bind(config: &InterviewConfig) -> bool {
    matches!(
        crate::interview::queue::load_queue_dir(&config.queue),
        Ok(questions) if !questions.is_empty()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_config() {
        let text = r#"# Interview config

session_id: design-interview-2026-08-23-1330
entity: c:\data\git\tod\doc\process\projects\tod\tasks\interview
phase: design-interview
transcript: c:\data\git\tod\doc\process\projects\tod\tasks\interview\history\design-interview-2026-08-23-1330.md
scope:
  - c:\data\git\tod\doc\process\projects\tod\tasks\interview\user.md
state_agent: C:\Users\joel\.claude\skills\process\agents\design-agent.md
scratchpad: c:\data\git\tod\.local\agent\process\projects\tod\tasks\interview\scratchpad\interviews\design-interview-2026-08-23-1330
queue: c:\data\git\tod\.local\agent\process\projects\tod\tasks\interview\scratchpad\interviews\design-interview-2026-08-23-1330\queue/
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
    fn strip_doc_process_mirrors_under_local_agent_process() {
        let repo = Path::new(r"c:\data\git\tod");
        let entity = repo
            .join("doc")
            .join("process")
            .join("projects")
            .join("tod")
            .join("tasks")
            .join("interview");
        // Without canonicalize (paths may not exist on CI), exercise the strip helper directly.
        let rel = Path::new("doc/process/projects/tod/tasks/interview");
        assert_eq!(
            strip_doc_process_prefix(rel),
            PathBuf::from("projects/tod/tasks/interview")
        );
        let _ = (repo, entity);
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

        let old_config_dir = agent_scratchpad_for_entity(repo, &entity_b)
            .unwrap()
            .join("old-session");
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
        let new_config_dir = agent_scratchpad_for_entity(repo, &entity)
            .unwrap()
            .join("new-session");
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
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_scaffolding_writes_sqlite_paths_when_config_appears() {
        use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
        use crate::interview::paths::TodPaths;
        use std::fs;

        let dir = std::env::temp_dir().join(format!("tod-sync-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.as_path();
        let entity = repo
            .join("doc")
            .join("process")
            .join("projects")
            .join("demo")
            .join("tasks")
            .join("t1");
        fs::create_dir_all(&entity).unwrap();

        let paths = TodPaths::from_repo_root(repo.to_path_buf());
        let store = SessionStore::open(&paths).unwrap();
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    display_name: "t1 — Initial".into(),
                    entity_path: entity.to_string_lossy().into(),
                    phase: "project-defining".into(),
                },
                InterviewSessionStatus::Active,
            )
            .unwrap();

        assert!(
            !sync_scaffolding_from_disk(&store, repo, session.id).unwrap(),
            "no config yet"
        );

        let scratch = agent_scratchpad_for_entity(repo, &entity)
            .unwrap()
            .join("project-defining-interview-test");
        fs::create_dir_all(scratch.join("queue")).unwrap();
        let config_path = scratch.join("interview-config.md");
        let transcript = entity
            .join("history")
            .join("project-defining-interview-test.md");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(&transcript, "# transcript\n").unwrap();
        fs::write(
            &config_path,
            format!(
                "session_id: project-defining-interview-test\nentity: {}\nphase: project-defining\ntranscript: {}\nscratchpad: {}\nqueue: {}/queue/\n",
                entity.display(),
                transcript.display(),
                scratch.display(),
                scratch.display(),
            ),
        )
        .unwrap();

        assert!(
            !sync_scaffolding_from_disk(&store, repo, session.id).unwrap(),
            "config alone (empty queue) should not bind yet"
        );

        fs::write(
            scratch.join("queue").join("q-001.md"),
            "---\nid: q-001\ncreated: 2026-08-25T12:00:00Z\n---\nFirst question?\n",
        )
        .unwrap();

        assert!(
            sync_scaffolding_from_disk(&store, repo, session.id).unwrap(),
            "config with queue questions should bind"
        );
        let mirrored = agent_scratchpad_for_entity(repo, &entity).unwrap();
        let mirrored_s = mirrored.to_string_lossy().replace('\\', "/");
        assert!(
            mirrored_s.ends_with("projects/demo/tasks/t1/scratchpad/interviews"),
            "mirrored path must drop doc/process, got {mirrored_s}"
        );

        let updated = store.get_session(session.id).unwrap().unwrap();
        assert_eq!(
            updated.session_id.as_deref(),
            Some("project-defining-interview-test")
        );
        assert_eq!(
            updated.config_path.as_deref(),
            Some(path_for_storage(&config_path).as_str())
        );
        assert_eq!(
            updated.scratchpad_path.as_deref(),
            Some(path_for_storage(&scratch).as_str())
        );
        assert!(sync_scaffolding_from_disk(&store, repo, session.id).unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_after_bootstrap_binds_empty_queue() {
        use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
        use crate::interview::paths::TodPaths;
        use std::fs;

        let dir = std::env::temp_dir().join(format!("tod-sync-empty-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let repo = dir.as_path();
        let entity = repo
            .join("doc")
            .join("process")
            .join("projects")
            .join("demo")
            .join("tasks")
            .join("t2");
        fs::create_dir_all(&entity).unwrap();

        let paths = TodPaths::from_repo_root(repo.to_path_buf());
        let store = SessionStore::open(&paths).unwrap();
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    display_name: "t2 — Initial".into(),
                    entity_path: entity.to_string_lossy().into(),
                    phase: "project-defining".into(),
                },
                InterviewSessionStatus::Active,
            )
            .unwrap();

        let scratch = agent_scratchpad_for_entity(repo, &entity)
            .unwrap()
            .join("empty-bootstrap");
        fs::create_dir_all(scratch.join("queue")).unwrap();
        let config_path = scratch.join("interview-config.md");
        let transcript = entity.join("history").join("empty-bootstrap.md");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(&transcript, "# transcript\n").unwrap();
        fs::write(
            &config_path,
            format!(
                "session_id: empty-bootstrap\nentity: {}\nphase: project-defining\ntranscript: {}\nscratchpad: {}\nqueue: {}/queue/\n",
                entity.display(),
                transcript.display(),
                scratch.display(),
                scratch.display(),
            ),
        )
        .unwrap();

        assert!(!sync_scaffolding_from_disk(&store, repo, session.id).unwrap());
        assert!(
            sync_scaffolding_from_disk_after_bootstrap(&store, repo, session.id).unwrap(),
            "after bootstrap, empty queue may still bind"
        );
        let updated = store.get_session(session.id).unwrap().unwrap();
        assert_eq!(updated.session_id.as_deref(), Some("empty-bootstrap"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_strips_windows_verbatim_prefix() {
        let text = r#"session_id: s1
entity: \\?\C:\repo\entity
phase: design-interview
transcript: \\?\C:\repo\entity\history\s1.md
scratchpad: \\?\C:\repo\.local\scratch\s1
queue: \\?\C:\repo\.local\scratch\s1\queue/
"#;
        let cfg = parse_interview_config_contents(Path::new("interview-config.md"), text).unwrap();
        assert!(!cfg.queue.to_string_lossy().contains(r"\\?\"));
        assert!(!cfg.entity.to_string_lossy().contains(r"\\?\"));
    }
}
