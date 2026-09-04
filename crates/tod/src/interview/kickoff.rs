use crate::interview::TodPaths;
use crate::process_bundle::{
    AgentLaunchContext, InterviewAgentPrompt, ProcessManifest, TodInstallPaths,
};
use std::path::Path;
use std::sync::Arc;
use tod_store::fleet::FleetStore;

pub fn question_maker_replenish_prompt(
    fleet: &Arc<FleetStore>,
    install: &TodInstallPaths,
    manifest: &ProcessManifest,
    paths: &TodPaths,
    node_id: uuid::Uuid,
    phase: &str,
    scratchpad: &Path,
    config_path: &Path,
    queue_target: u32,
    agent_config_id: Option<&str>,
) -> anyhow::Result<InterviewAgentPrompt> {
    let fleet_projection = fleet.projection();
    let guard = fleet_projection.lock().expect("fleet projection mutex");
    let conn = guard.connection();
    let instruction = format!(
        "Go after the queue for interview session.\n\
         Target open question count: {queue_target}\n\
         Follow the question maker role instructions. Return queue directory path only."
    );
    let ctx = AgentLaunchContext::question_maker_followup(
        &conn,
        install,
        manifest,
        paths,
        node_id,
        phase,
        scratchpad,
        config_path,
        &instruction,
        agent_config_id,
    )?;
    Ok(ctx.prompt)
}

pub fn answer_processor_prompt(
    fleet: &Arc<FleetStore>,
    install: &TodInstallPaths,
    manifest: &ProcessManifest,
    paths: &TodPaths,
    node_id: uuid::Uuid,
    phase: &str,
    scratchpad: &Path,
    config_path: &Path,
    payload: &str,
    agent_config_id: Option<&str>,
) -> anyhow::Result<InterviewAgentPrompt> {
    let fleet_projection = fleet.projection();
    let guard = fleet_projection.lock().expect("fleet projection mutex");
    let conn = guard.connection();
    let instruction = format!(
        "Process interview answer submission.\n\
         The UI already appended Q&A to the entity transcript.\n\
         \n\
         Answer payload (YAML multi-record):\n\
         {payload}\n\
         \n\
         Reply with resolved:/modified: id lists only."
    );
    let ctx = AgentLaunchContext::answer_processor(
        &conn,
        install,
        manifest,
        paths,
        node_id,
        phase,
        scratchpad,
        config_path,
        &instruction,
        agent_config_id,
    )?;
    Ok(ctx.prompt)
}

pub fn question_maker_action_prompt(
    fleet: &Arc<FleetStore>,
    install: &TodInstallPaths,
    manifest: &ProcessManifest,
    paths: &TodPaths,
    node_id: uuid::Uuid,
    phase: &str,
    scratchpad: &Path,
    config_path: &Path,
    payload: &str,
    agent_config_id: Option<&str>,
) -> anyhow::Result<InterviewAgentPrompt> {
    let fleet_projection = fleet.projection();
    let guard = fleet_projection.lock().expect("fleet projection mutex");
    let conn = guard.connection();
    let instruction = format!(
        "Process question maker action submission.\n\
         The UI already appended the action to the entity transcript.\n\
         \n\
         Action payload (YAML multi-record):\n\
         {payload}\n\
         \n\
         Delete or modify queue files per action semantics. Return queue directory path only."
    );
    let ctx = AgentLaunchContext::question_maker_followup(
        &conn,
        install,
        manifest,
        paths,
        node_id,
        phase,
        scratchpad,
        config_path,
        &instruction,
        agent_config_id,
    )?;
    Ok(ctx.prompt)
}
