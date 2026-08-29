use crate::interview::config::agent_scratchpad_for_entity;
use crate::interview::{InterviewSession, TodPaths};
use std::path::Path;

/// Build the researcher kickoff prompt for bootstrap + initial queue population.
pub fn researcher_bootstrap_prompt(session: &InterviewSession) -> String {
    let entity = session.entity_path.as_deref().unwrap_or("(unknown entity)");
    let phase = session.phase.as_deref().unwrap_or("(unknown phase)");
    let mut abs_hints = String::new();
    let mut skill_hint = String::new();
    if let Ok(paths) = TodPaths::discover() {
        let skill = paths
            .repo_root()
            .join("refs")
            .join("process")
            .join("interview")
            .join("SKILL.md");
        let researcher = paths
            .repo_root()
            .join("refs")
            .join("process")
            .join("interview")
            .join("agents")
            .join("interview-researcher-agent.md");
        skill_hint = format!(
            "\n\
         Read these files first (absolute paths):\n\
         - {}\n\
         - {}\n",
            skill.display(),
            researcher.display(),
        );
        if let Ok(entity_path) = std::path::PathBuf::from(entity)
            .canonicalize()
            .or_else(|_| Ok::<_, std::io::Error>(std::path::PathBuf::from(entity)))
        {
            if let Ok(scratch_root) = agent_scratchpad_for_entity(paths.repo_root(), &entity_path) {
                abs_hints = format!(
                    "\n\
         Absolute paths (required — do not invent roots; do not use \\\\?\\ prefixes):\n\
         - Repo/data root: {}\n\
         - Entity: {}\n\
         - Transcript dir: {}\\history\\\n\
         - Session scratchpad dir: {}\\{{session-id}}\\\n\
         (Create queue/ and interview-config.md inside that session scratchpad dir.)\n",
                    paths.repo_root().display(),
                    entity_path.display(),
                    entity_path.display(),
                    scratch_root.display(),
                );
            }
        }
    }

    format!(
        "Interview UI kickoff — bootstrap this interview session.\n\
         \n\
         SQLite session id: {}\n\
         Display name: {}\n\
         Entity path: {entity}\n\
         Phase/purpose: {phase}\n\
         {skill_hint}\
         {abs_hints}\
         Follow the interview SKILL bootstrap (researcher owns scaffolding):\n\
         1. Create transcript under {{entity}}/history/{{description}}-{{YYYY-MM-DD}}-{{HHMM}}.md (session header only).\n\
         2. Derive session_id from transcript filename stem.\n\
         3. Create session scratchpad under the Absolute paths session scratchpad parent above.\n\
         4. Create empty queue/ and interview-config.md with absolute paths (no \\\\?\\ prefixes).\n\
         5. Populate queue/ with initial questions for this phase (at least several open questions before you finish).\n\
         6. Update researcher-status.md only after queue files exist.\n\
         \n\
         Queue files use YAML front matter only for UI fields (context, question, recommend, proposed_text, options with digit keys \"1\"/\"2\"/\"3\", layer, kind, covers); leave the markdown body empty. Never duplicate MC labels or Recommend lines in the body.\n\
         Do not talk to the user. Do not return until queue/ has question files. Return the queue directory path only when done.",
        session.id, session.display_name,
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
