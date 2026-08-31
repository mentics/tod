//! One-time bootstrap import from `doc/process/` on disk.

use crate::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo};
use crate::outline::repos::obligations::NodeObligation;
use crate::outline::types::Capability;
use crate::outline::uuid_blob::{now_ms, uuid_to_blob};
use crate::fleet::repos::agent_config::AgentConfigRepo;
use anyhow::Result;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use uuid::Uuid;

pub struct ImportReport {
    pub list_id: Uuid,
    pub projects: usize,
    pub tasks: usize,
    pub global_obligations: usize,
}

/// Import `doc/process` under `repo_root` into the outline schema.
pub fn import_doc_process(conn: &Connection, repo_root: &Path, media_root: &Path) -> Result<ImportReport> {
    let list_repo = ListRepo::new(conn);
    let list = list_repo
        .get_by_slug("tod")?
        .unwrap_or_else(|| list_repo.create("tod", "tod").expect("create list"));

    let mut projects = 0usize;
    let mut tasks = 0usize;

    let projects_root = repo_root.join("doc").join("process").join("projects");
    if projects_root.is_dir() {
        for project_entry in fs::read_dir(&projects_root)?.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }
            let project_slug = project_entry
                .file_name()
                .to_string_lossy()
                .to_string();
            let project_node = import_project(conn, &list.id, &project_path, &project_slug)?;
            projects += 1;

            let tasks_dir = project_path.join("tasks");
            if tasks_dir.is_dir() {
                for task_entry in fs::read_dir(&tasks_dir)?.flatten() {
                    let task_path = task_entry.path();
                    if !task_path.is_dir() {
                        continue;
                    }
                    if !task_path.join("state.md").is_file() {
                        continue;
                    }
                    let task_slug = task_entry.file_name().to_string_lossy().to_string();
                    import_task(
                        conn,
                        &list.id,
                        project_node.id,
                        &task_path,
                        &task_slug,
                        media_root,
                    )?;
                    tasks += 1;
                }
            }
        }
    }

    let global_count = import_global_obligations(conn, repo_root)?;

    // Reattach legacy agents by slug match.
    reattach_agents_by_slug(conn)?;

    Ok(ImportReport {
        list_id: list.id,
        projects,
        tasks,
        global_obligations: global_count,
    })
}

fn import_project(
    conn: &Connection,
    list_id: &Uuid,
    project_path: &Path,
    project_slug: &str,
) -> Result<crate::outline::types::Node> {
    let node_repo = NodeRepo::new(conn);
    let outline = OutlineRepo::new(conn);

    if let Some(existing) = node_repo.get_by_slug(project_slug)? {
        return Ok(existing);
    }

    let title = read_title(&project_path.join("user.md")).unwrap_or_else(|| project_slug.to_string());
    let node = node_repo.create_normal(project_slug, &title)?;
    node_repo.enable_capabilities(node.id, &[Capability::Spec, Capability::Lifecycle])?;

    let ordinal = outline.next_ordinal(*list_id, None)?;
    outline.insert(&crate::outline::types::OutlineEntry {
        node_id: node.id,
        list_id: *list_id,
        parent_id: None,
        ordinal,
        collapsed: false,
    })?;

    if let Ok(body) = fs::read_to_string(project_path.join("user.md")) {
        parse_user_md_into_node(conn, node.id, &body)?;
    }
    if let Ok(state) = fs::read_to_string(project_path.join("state.md")) {
        if let Some(lc) = parse_lifecycle(&state) {
            node_repo.set_lifecycle(node.id, &lc)?;
        }
    }
    for doc in ["design", "plan"] {
        let path = project_path.join(format!("{doc}.md"));
        if path.is_file() {
            let body = fs::read_to_string(&path)?;
            insert_extra_content(conn, node.id, doc, &body)?;
        }
    }

    Ok(node)
}

fn import_task(
    conn: &Connection,
    list_id: &Uuid,
    parent_id: Uuid,
    task_path: &Path,
    task_slug: &str,
    media_root: &Path,
) -> Result<()> {
    let node_repo = NodeRepo::new(conn);
    let outline = OutlineRepo::new(conn);

    let node = if let Some(existing) = node_repo.get_by_slug(task_slug)? {
        existing
    } else {
        let title = read_title(&task_path.join("user.md")).unwrap_or_else(|| task_slug.to_string());
        let node = node_repo.create_normal(task_slug, &title)?;
        node_repo.enable_capabilities(
            node.id,
            &[Capability::Spec, Capability::Lifecycle, Capability::Agent],
        )?;
        node
    };

    if outline.get_entry(node.id)?.is_none() {
        let ordinal = outline.next_ordinal(*list_id, Some(parent_id))?;
        outline.insert(&crate::outline::types::OutlineEntry {
            node_id: node.id,
            list_id: *list_id,
            parent_id: Some(parent_id),
            ordinal,
            collapsed: false,
        })?;
    }

    if let Ok(body) = fs::read_to_string(task_path.join("user.md")) {
        parse_user_md_into_node(conn, node.id, &body)?;
    }
    if let Ok(state) = fs::read_to_string(task_path.join("state.md")) {
        if let Some(lc) = parse_lifecycle(&state) {
            node_repo.set_lifecycle(node.id, &lc)?;
        }
    }
    for doc in ["design", "plan"] {
        let path = task_path.join(format!("{doc}.md"));
        if path.is_file() {
            let body = fs::read_to_string(&path)?;
            insert_extra_content(conn, node.id, doc, &body)?;
        }
    }

    import_history(conn, node.id, &task_path.join("history"))?;
    import_visual_artifacts(conn, node.id, &task_path.join("artifacts").join("visual"), media_root)?;

    Ok(())
}

fn import_global_obligations(conn: &Connection, repo_root: &Path) -> Result<usize> {
    let dir = repo_root
        .join("doc")
        .join("process")
        .join("shared")
        .join("constraints");
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let slug = path.file_stem().unwrap().to_string_lossy().to_string();
        if slug == "README" {
            continue;
        }
        let body = fs::read_to_string(&path)?;
        let title = slug.clone();
        let id = Uuid::new_v4();
        conn.execute(
            "INSERT OR IGNORE INTO global_obligations (id, slug, title, kind, ordinal, body, adopted)
             VALUES (?1, ?2, ?3, 'constraint', 1, ?4, 1)",
            rusqlite::params![uuid_to_blob(id), slug, title, body],
        )?;
        count += 1;
    }
    Ok(count)
}

fn parse_user_md_into_node(conn: &Connection, node_id: Uuid, body: &str) -> Result<()> {
    let mut in_requirements = false;
    let mut in_constraints = false;
    let mut req_ordinal = 0i32;
    let mut con_ordinal = 0i32;
    let obl_repo = ObligationRepo::new(conn);

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Requirements") {
            in_requirements = true;
            in_constraints = false;
            continue;
        }
        if trimmed.starts_with("## Constraints") {
            in_constraints = true;
            in_requirements = false;
            continue;
        }
        if trimmed.starts_with("## ") {
            in_requirements = false;
            in_constraints = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            insert_extra_content(conn, node_id, "goal", rest)?;
            continue;
        }
        let Some(num_end) = trimmed.find('.') else {
            continue;
        };
        let num_part = &trimmed[..num_end];
        if num_part.chars().all(|c| c.is_ascii_digit()) {
            let text = trimmed[num_end + 1..].trim();
            if text.is_empty() {
                continue;
            }
            if in_requirements {
                req_ordinal += 1;
                obl_repo.insert(&NodeObligation {
                    id: Uuid::new_v4(),
                    node_id,
                    kind: "requirement".into(),
                    ordinal: req_ordinal,
                    section: None,
                    body: text.to_string(),
                })?;
            } else if in_constraints {
                con_ordinal += 1;
                obl_repo.insert(&NodeObligation {
                    id: Uuid::new_v4(),
                    node_id,
                    kind: "constraint".into(),
                    ordinal: con_ordinal,
                    section: None,
                    body: text.to_string(),
                })?;
            }
        }
    }
    Ok(())
}

fn insert_extra_content(conn: &Connection, node_id: Uuid, content_type: &str, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO node_extra_content (id, node_id, content_type, body, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(node_id, content_type) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        rusqlite::params![
            uuid_to_blob(Uuid::new_v4()),
            uuid_to_blob(node_id),
            content_type,
            body,
            now_ms()
        ],
    )?;
    Ok(())
}

fn import_history(conn: &Connection, node_id: Uuid, history_dir: &Path) -> Result<()> {
    if !history_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(history_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let display_name = path.file_stem().unwrap().to_string_lossy().to_string();
        let body = fs::read_to_string(&path)?;
        let phase = display_name.split('-').next().unwrap_or("interview").to_string();
        conn.execute(
            "INSERT INTO interview_transcripts (id, node_id, phase, display_name, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                uuid_to_blob(Uuid::new_v4()),
                uuid_to_blob(node_id),
                phase,
                display_name,
                body,
                now_ms()
            ],
        )?;
    }
    Ok(())
}

fn import_visual_artifacts(
    conn: &Connection,
    node_id: Uuid,
    visual_dir: &Path,
    media_root: &Path,
) -> Result<()> {
    if !visual_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(visual_dir)?.flatten() {
        let package_dir = entry.path();
        if !package_dir.is_dir() {
            continue;
        }
        for file in fs::read_dir(&package_dir)?.flatten() {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            let bytes = fs::read(&path)?;
            let media_id = Uuid::new_v4();
            let filename = path.file_name().unwrap().to_string_lossy();
            let relative = format!("{media_id}/{filename}");
            let dest_dir = media_root.join(media_id.to_string());
            fs::create_dir_all(&dest_dir)?;
            fs::copy(&path, dest_dir.join(path.file_name().unwrap()))?;
            let sha = format!("{:x}", sha256_simple(&bytes));
            conn.execute(
                "INSERT OR IGNORE INTO media_assets (id, relative_path, content_type, byte_size, sha256, created_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
                rusqlite::params![
                    uuid_to_blob(media_id),
                    relative,
                    bytes.len() as i64,
                    sha,
                    now_ms()
                ],
            )?;
            let role = path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            conn.execute(
                "INSERT OR IGNORE INTO node_media_links (node_id, media_id, role, label, ordinal)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![uuid_to_blob(node_id), uuid_to_blob(media_id), role, role],
            )?;
        }
    }
    Ok(())
}

fn sha256_simple(bytes: &[u8]) -> u64 {
    // Lightweight placeholder hash for import (not cryptographic).
    bytes.iter().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64))
}

fn reattach_agents_by_slug(conn: &Connection) -> Result<()> {
    let agent_repo = AgentConfigRepo::new(conn);
    let agents = agent_repo.list_all()?;
    for agent in agents {
        // Already on node_id after v3 migration.
        let _ = agent;
    }
    Ok(())
}

fn read_title(user_md: &Path) -> Option<String> {
    let body = fs::read_to_string(user_md).ok()?;
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

fn parse_lifecycle(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("- State:") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}
