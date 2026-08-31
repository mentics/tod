//! App-local log of interview question feedback (not sent to agents).

use crate::interview::TodPaths;
use anyhow::{Context, Result};
use chrono::Local;
use std::fs::OpenOptions;
use std::io::Write;

pub fn question_feedback_path(paths: &TodPaths) -> std::path::PathBuf {
    paths.data_root().join("question-feedback.md")
}

/// Append one feedback entry under the data root.
pub fn append_question_feedback(
    paths: &TodPaths,
    question_id: &str,
    node_title: &str,
    lifecycle_state: &str,
    feedback: &str,
    raw_question: &str,
) -> Result<()> {
    let path = question_feedback_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create question-feedback parent {}",
                parent.display()
            )
        })?;
    }

    let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let mut block = format!(
        "\n## {question_id} ({timestamp})\n\n\
         Node: {node_title} | Lifecycle: {lifecycle_state}\n\n\
         {feedback}\n\n\
         Raw question:\n```\n{raw_question}\n```\n"
    );
    if !block.ends_with('\n') {
        block.push('\n');
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(block.as_bytes())
        .with_context(|| format!("failed to append {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interview::paths::{clear_data_root_override, set_data_root};

    #[test]
    fn append_writes_under_data_root() {
        let root = std::env::temp_dir().join(format!("tod-qfb-{}", uuid::Uuid::new_v4()));
        set_data_root(root.clone());
        let paths = TodPaths::discover().unwrap();
        append_question_feedback(
            &paths,
            "q-001",
            "Header redesign",
            "proposed",
            "Too meta — obvious default is yes.",
            "---\nid: q-001\n---\n",
        )
        .unwrap();
        let text = std::fs::read_to_string(question_feedback_path(&paths)).unwrap();
        assert!(text.contains("Header redesign"));
        assert!(text.contains("proposed"));
        assert!(text.contains("Too meta"));
        assert!(text.contains("id: q-001"));
        clear_data_root_override();
    }
}
