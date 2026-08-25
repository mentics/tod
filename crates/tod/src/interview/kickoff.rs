use crate::interview::InterviewSession;
use std::path::Path;

/// Build the researcher kickoff prompt for bootstrap + initial queue population.
pub fn researcher_bootstrap_prompt(session: &InterviewSession) -> String {
    format!(
        "Interview UI kickoff — bootstrap this interview session.\n\
         \n\
         SQLite session id: {}\n\
         Display name: {}\n\
         Entity path: {}\n\
         Phase/purpose: {}\n\
         \n\
         Follow refs/process/interview/SKILL.md bootstrap (researcher owns scaffolding):\n\
         1. Create transcript under {{entity}}/history/{{description}}-{{YYYY-MM-DD}}-{{HHMM}}.md (session header only).\n\
         2. Derive session_id from transcript filename stem.\n\
         3. Create session scratchpad under .local/agent/process/{{mirrored entity}}/scratchpad/interviews/{{session-id}}/\n\
         4. Create empty queue/ and interview-config.md with absolute paths.\n\
         5. Populate queue/ with initial questions for this phase.\n\
         6. Update researcher-status.md.\n\
         \n\
         Queue files use YAML front matter + markdown body; MC options as options: key/label list.\n\
         Do not talk to the user. Return the queue directory path only when done.",
        session.id,
        session.display_name,
        session.entity_path.as_deref().unwrap_or("(unknown entity)"),
        session.phase.as_deref().unwrap_or("(unknown phase)"),
    )
}

/// Build replenishment prompt for an existing session.
pub fn researcher_replenish_prompt(config_path: &Path, queue_target: u32) -> String {
    format!(
        "Go after the queue for interview session.\n\
         Config path: {}\n\
         Target open question count: {queue_target}\n\
         Follow interview-researcher-agent.md. Return queue directory path only.",
        config_path.display()
    )
}

/// Build answer-processor prompt with YAML payload after UI transcript append.
pub fn answer_processor_prompt(config_path: &Path, payload: &str) -> String {
    format!(
        "Process interview answer submission.\n\
         Config path: {}\n\
         The UI already appended Q&A to the entity transcript.\n\
         \n\
         Answer payload (YAML multi-record):\n\
         {}\n\
         \n\
         Reply with resolved:/modified: id lists only.",
        config_path.display(),
        payload
    )
}

/// Build researcher action prompt after UI transcript append.
pub fn researcher_action_prompt(config_path: &Path, payload: &str) -> String {
    format!(
        "Process researcher action submission.\n\
         Config path: {}\n\
         The UI already appended the action to the entity transcript.\n\
         \n\
         Action payload (YAML multi-record):\n\
         {}\n\
         \n\
         Delete or modify queue files per action semantics. Return queue directory path only.",
        config_path.display(),
        payload
    )
}
