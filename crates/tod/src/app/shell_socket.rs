//! Agent control socket handlers for shell launch/focus (no UI thread required).

use crate::agent_socket::commands::ShellSocketCommand;
use crate::interview::{TodPaths, TodSettings};
use std::sync::Arc;
use tod_store::fleet::terminal::{ShellState, read_shell_state, shells_dir, state_file_path};
use tod_store::fleet::{
    FleetStore, focus_shell_session, open_shell_for_agent_config, verify_shell_session,
};

#[derive(Clone)]
pub struct ShellSocketService {
    fleet: Arc<FleetStore>,
    paths: TodPaths,
}

impl ShellSocketService {
    pub fn new(fleet: Arc<FleetStore>, paths: TodPaths) -> Self {
        Self { fleet, paths }
    }

    pub fn handle(&self, action: ShellSocketCommand) -> Result<String, String> {
        let settings = TodSettings::load(&self.paths).map_err(|err| err.to_string())?;
        match action {
            ShellSocketCommand::Launch { task_id, config_id } => {
                let (shell_id, cwd) = open_shell_for_agent_config(
                    &self.fleet,
                    &self.paths,
                    &settings,
                    &config_id,
                    &task_id,
                )
                .map_err(|err| err.to_string())?;
                let pid = read_shell_state(&state_file_path(&shells_dir(&self.paths), &shell_id))
                    .ok()
                    .map(|state: ShellState| state.pid)
                    .unwrap_or(0);
                Ok(format!("ok {shell_id} pid={pid} cwd={}", cwd.display()))
            }
            ShellSocketCommand::Verify { shell_id } => {
                let (alive, pid) =
                    verify_shell_session(&self.paths, &shell_id).map_err(|err| err.to_string())?;
                if alive {
                    Ok(format!("ok alive pid={}", pid.unwrap_or(0)))
                } else {
                    Ok(format!("ok dead pid={}", pid.unwrap_or(0)))
                }
            }
            ShellSocketCommand::Focus { task_id, shell_id } => {
                let shell = self
                    .fleet
                    .get_shell(&shell_id)
                    .map_err(|err| err.to_string())?
                    .ok_or_else(|| format!("shell session {shell_id} not found"))?;
                let cwd = focus_shell_session(
                    &self.fleet,
                    &self.paths,
                    &settings,
                    &shell.agent_id,
                    &task_id,
                    &shell,
                )
                .map_err(|err| err.to_string())?;
                Ok(format!("ok focused cwd={}", cwd.display()))
            }
        }
    }
}
