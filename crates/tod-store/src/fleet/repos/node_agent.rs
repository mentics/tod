//! Agent capability repository — platform / model / effort per node.
//!
//! Each value is optional; unset values follow the settings for the launch role.

use crate::agent_launch::{
    AgentLaunchOptions, DEFAULT_EFFORT, coerce_effort, coerce_model, default_model_for,
    parse_platform,
};
use crate::fleet::repos::{node_id_blob, node_id_column};
use crate::outline::uuid_blob::now_ms;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeAgent {
    pub node_id: String,
    pub platform: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl NodeAgent {
    /// Launch options with unset values taken from `fallback` (the settings
    /// for the launch role). When this node picks a different platform than
    /// the fallback, unset model / effort use that platform's defaults.
    pub fn launch_options(&self, fallback: &AgentLaunchOptions) -> AgentLaunchOptions {
        let platform = non_empty(&self.platform)
            .and_then(parse_platform)
            .unwrap_or(fallback.platform);
        let same_platform = platform == fallback.platform;
        let model = match non_empty(&self.model) {
            Some(model) => model.to_string(),
            None if same_platform => fallback.model.clone(),
            None => default_model_for(platform).to_string(),
        };
        let effort = match non_empty(&self.effort) {
            Some(effort) => effort.to_string(),
            None if same_platform => fallback.effort.clone(),
            None => DEFAULT_EFFORT.to_string(),
        };
        AgentLaunchOptions {
            platform,
            model: coerce_model(platform, &model),
            effort: coerce_effort(platform, &effort),
        }
    }
}

pub struct NodeAgentRepo<'a> {
    conn: &'a Connection,
}

impl<'a> NodeAgentRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn get(&self, node_id: &str) -> Result<Option<NodeAgent>> {
        let blob = node_id_blob(node_id)?;
        self.conn
            .query_row(
                "SELECT node_id, platform, model, effort FROM node_agent WHERE node_id = ?1",
                params![blob],
                |row| {
                    Ok(NodeAgent {
                        node_id: node_id_column(row, 0)?,
                        platform: row.get(1)?,
                        model: row.get(2)?,
                        effort: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert(
        &self,
        node_id: &str,
        platform: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<()> {
        let blob = node_id_blob(node_id)?;
        self.conn.execute(
            "INSERT INTO node_agent (node_id, platform, model, effort, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id) DO UPDATE SET
               platform = excluded.platform, model = excluded.model,
               effort = excluded.effort, updated_at = excluded.updated_at",
            params![blob, platform, model, effort, now_ms()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::{cleanup_test_dir, seed_node, test_writer_conn};
    use crate::settings::AgentPlatform;

    #[test]
    fn unset_values_follow_fallback() {
        let fallback = AgentLaunchOptions::from_settings(AgentPlatform::Claude, "opus", "high");
        let agent = NodeAgent::default();
        assert_eq!(agent.launch_options(&fallback), fallback);

        let agent = NodeAgent {
            model: Some("sonnet".into()),
            ..NodeAgent::default()
        };
        let opts = agent.launch_options(&fallback);
        assert_eq!(opts.model, "sonnet");
        assert_eq!(opts.effort, "high");

        let agent = NodeAgent {
            platform: Some("cursor".into()),
            ..NodeAgent::default()
        };
        let opts = agent.launch_options(&fallback);
        assert_eq!(opts.platform, AgentPlatform::Cursor);
        assert_eq!(opts.model, "auto");
        assert_eq!(opts.effort, "auto");
    }

    #[test]
    fn upsert_round_trip() {
        let (dir, conn) = test_writer_conn();
        let node_id = seed_node(&conn);
        let repo = NodeAgentRepo::new(&conn);
        repo.upsert(&node_id, Some("cursor"), None, Some("low"))
            .unwrap();
        let agent = repo.get(&node_id).unwrap().unwrap();
        assert_eq!(agent.platform.as_deref(), Some("cursor"));
        assert_eq!(agent.model, None);
        assert_eq!(agent.effort.as_deref(), Some("low"));
        cleanup_test_dir(&dir);
    }
}
