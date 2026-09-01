use crate::interview::db::{InterviewSession, SessionStore};
use crate::interview::paths::TodPaths;
use crate::process_bundle::node_scratchpad_root;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterviewConfig {
    pub session_id: String,
    pub node_id: Uuid,
    pub phase: String,
    pub scratchpad: PathBuf,
    pub queue: PathBuf,
    pub config_path: PathBuf,
    pub queue_target: Option<u32>,
    pub role_doc: Option<PathBuf>,
    pub scope: Vec<PathBuf>,
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
    let node_id = values
        .get("node_id")
        .context("interview-config missing node_id")?
        .parse::<Uuid>()
        .context("invalid node_id UUID")?;
    let phase = values
        .get("phase")
        .cloned()
        .context("interview-config missing phase")?;
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
        node_id,
        phase,
        scratchpad,
        queue,
        config_path: path.to_path_buf(),
        queue_target,
        role_doc: values
            .get("role_doc")
            .map(|v| PathBuf::from(normalize_config_path(v))),
        scope,
    })
}

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

pub fn base_interview_phase(phase: &str) -> &str {
    phase.split('(').next().unwrap_or(phase).trim()
}

pub use tod_store::path_for_storage;

pub fn agent_scratchpad_for_node(data_root: &Path, node_id: Uuid) -> PathBuf {
    node_scratchpad_root(data_root, node_id)
}

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

pub fn find_bootstrap_config_for_session(
    data_root: &Path,
    session: &InterviewSession,
) -> Result<Option<PathBuf>> {
    let kickoff = session.created_at;
    let base_phase = base_interview_phase(&session.phase);

    let mut search_roots = vec![agent_scratchpad_for_node(data_root, session.node_id)];
    search_roots.push(
        data_root
            .join(".local")
            .join("agent")
            .join("nodes")
            .join(session.node_id.to_string()),
    );
    // Pre-fix bootstrap wrote under git checkout root instead of data root.
    if let Ok(paths) = TodPaths::discover() {
        let repo = paths.repo_root();
        if repo != data_root {
            search_roots.push(
                repo.join("agent")
                    .join("nodes")
                    .join(session.node_id.to_string())
                    .join("scratchpad")
                    .join("interviews"),
            );
        }
    }

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
            if config.node_id != session.node_id {
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

pub fn sync_scaffolding_from_disk(
    store: &SessionStore,
    data_root: &Path,
    session_id: Uuid,
) -> Result<bool> {
    let session = store
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("session {session_id} not found"))?;
    if session
        .scratchpad_path
        .as_ref()
        .is_some_and(|p| {
            let path = Path::new(p);
            tod_store::path_is_under(data_root, path)
                && path.join("interview-config.md").exists()
        })
    {
        return Ok(true);
    }

    let Some(config_path) = find_bootstrap_config_for_session(data_root, &session)? else {
        return Ok(false);
    };
    let config = parse_interview_config(&config_path)?;
    if config.node_id != session.node_id {
        return Ok(false);
    }
    if !scaffolding_ready_to_bind(&config) {
        return Ok(false);
    }
    if !tod_store::path_is_under(data_root, &config.scratchpad) {
        tracing::warn!(
            event = "interview",
            action = "reject_scratchpad_outside_data_root",
            scratchpad = %config.scratchpad.display(),
            data_root = %data_root.display(),
            "ignoring interview scaffolding outside data root"
        );
        return Ok(false);
    }
    bind_session_scaffolding(store, session_id, &config_path, &config)
}

pub fn sync_scaffolding_from_disk_after_bootstrap(
    store: &SessionStore,
    data_root: &Path,
    session_id: Uuid,
) -> Result<bool> {
    let session = store
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("session {session_id} not found"))?;
    if session
        .scratchpad_path
        .as_ref()
        .is_some_and(|p| {
            let path = Path::new(p);
            tod_store::path_is_under(data_root, path)
                && path.join("interview-config.md").exists()
        })
    {
        return Ok(true);
    }

    let Some(config_path) = find_bootstrap_config_for_session(data_root, &session)? else {
        return Ok(false);
    };
    let config = parse_interview_config(&config_path)?;
    if config.node_id != session.node_id {
        return Ok(false);
    }
    if !tod_store::path_is_under(data_root, &config.scratchpad) {
        tracing::warn!(
            event = "interview",
            action = "reject_scratchpad_outside_data_root",
            scratchpad = %config.scratchpad.display(),
            data_root = %data_root.display(),
            "ignoring interview scaffolding outside data root"
        );
        return Ok(false);
    }
    bind_session_scaffolding(store, session_id, &config_path, &config)
}

fn bind_session_scaffolding(
    store: &SessionStore,
    session_id: Uuid,
    _config_path: &Path,
    config: &InterviewConfig,
) -> Result<bool> {
    store.update_session_scaffolding(
        session_id,
        Some(&config.session_id),
        Some(&path_for_storage(&config.scratchpad)),
    )?;
    Ok(true)
}

fn scaffolding_ready_to_bind(config: &InterviewConfig) -> bool {
    matches!(
        crate::interview::queue::load_queue_dir(&config.queue),
        Ok(questions) if !questions.is_empty()
    )
}

fn system_time_to_utc(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_store::fleet::FleetStore;
    use crate::interview::db::{InterviewSessionStatus, NewInterviewSession, SessionStore};
    use tod_store::outline::OutlineMutation;
    use std::fs;

    fn test_fleet() -> (PathBuf, std::sync::Arc<FleetStore>, Uuid) {
        let root = std::env::temp_dir().join(format!("tod-cfg-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let fleet = std::sync::Arc::new(FleetStore::open(&root).unwrap());
        fleet
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "t".into(),
                title: "T".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "Node".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let node_id = fleet.flatten_outline(list_id).unwrap()[0].node.id;
        (root, fleet, node_id)
    }

    #[test]
    fn parses_node_config() {
        let text = r#"# Interview config

session_id: design-interview-2026-08-23-1330
node_id: 550e8400-e29b-41d4-a716-446655440000
phase: design-interview
role_doc: C:\install\process\agents\state\design.md
scope:
  - C:\scratch\scope\obligations.md
scratchpad: C:\scratch\session
queue: C:\scratch\session\queue/
queue_target: 8
"#;
        let cfg = parse_interview_config_contents(Path::new("interview-config.md"), text).unwrap();
        assert_eq!(cfg.session_id, "design-interview-2026-08-23-1330");
        assert_eq!(cfg.scope.len(), 1);
        assert_eq!(cfg.queue_target, Some(8));
    }

    #[test]
    fn sync_scaffolding_binds_when_queue_ready() {
        let (root, fleet, node_id) = test_fleet();
        let store = SessionStore::open(fleet);
        let session = store
            .insert_session_with_metadata(
                NewInterviewSession {
                    node_id,
                    agent_config_id: None,
                    display_name: "Test".into(),
                    phase: "design-interview".into(),
                },
                InterviewSessionStatus::Active,
                None,
            )
            .unwrap();

        let scratch = agent_scratchpad_for_node(&root, node_id).join("sess-1");
        fs::create_dir_all(scratch.join("queue")).unwrap();
        let config_path = scratch.join("interview-config.md");
        fs::write(
            &config_path,
            format!(
                "session_id: sess-1\nnode_id: {node_id}\nphase: design-interview\nscratchpad: {}\nqueue: {}/queue/\n",
                scratch.display(),
                scratch.display(),
            ),
        )
        .unwrap();
        fs::write(
            scratch.join("queue").join("q-001.md"),
            "---\nid: q-001\n---\nQ?\n",
        )
        .unwrap();

        assert!(sync_scaffolding_from_disk(&store, &root, session.id).unwrap());
        let updated = store.get_session(session.id).unwrap().unwrap();
        assert_eq!(updated.session_id.as_deref(), Some("sess-1"));
        let _ = fs::remove_dir_all(root);
    }
}
