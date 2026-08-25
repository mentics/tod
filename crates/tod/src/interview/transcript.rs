use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Append an answer Q&A block to the entity transcript before invoking answer-processor.
pub fn append_answer(
    transcript_path: &Path,
    question_id: &str,
    question_body: &str,
    answer_text: &str,
    selected_option: Option<&str>,
) -> Result<()> {
    let mut block = format!("\n## {question_id}\n\n{question_body}\n\n");
    if let Some(option) = selected_option {
        block.push_str(&format!("**Selected:** {option}\n\n"));
    }
    block.push_str(&format!("**Answer:** {answer_text}\n"));
    append_block(transcript_path, &block)
}

/// Append a researcher-action block before forwarding to the researcher.
pub fn append_action(
    transcript_path: &Path,
    question_id: &str,
    action: &str,
    notes: Option<&str>,
    question_body: Option<&str>,
) -> Result<()> {
    let mut block = format!("\n## {question_id} (action: {action})\n\n");
    if let Some(body) = question_body {
        block.push_str(body);
        block.push('\n');
        block.push('\n');
    }
    if let Some(notes) = notes.filter(|s| !s.trim().is_empty()) {
        block.push_str(notes);
        block.push('\n');
    }
    append_block(transcript_path, &block)
}

fn append_block(transcript_path: &Path, block: &str) -> Result<()> {
    if let Some(parent) = transcript_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create transcript parent dir {}",
                parent.display()
            )
        })?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript_path)
        .with_context(|| format!("failed to open transcript {}", transcript_path.display()))?;
    file.write_all(block.as_bytes())
        .with_context(|| format!("failed to append transcript {}", transcript_path.display()))?;
    Ok(())
}

/// Build a new transcript filename per interview SKILL rules.
pub fn new_transcript_filename(description: &str, at: DateTime<Local>) -> String {
    format!(
        "{}-{}-{}.md",
        slugify(description),
        at.format("%Y-%m-%d"),
        at.format("%H%M")
    )
}

pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_hyphen = false;
        } else if !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnswerRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionRecord {
    pub action: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

/// Serialize one or more answer records into the YAML multi-record payload shape.
pub fn format_answer_payload(records: &[AnswerRecord]) -> Result<String> {
    if records.is_empty() {
        bail!("answer payload requires at least one record");
    }
    let mut out = String::new();
    for record in records {
        let header = serde_yaml::to_string(record).context("failed to serialize answer record")?;
        out.push_str("---\n");
        out.push_str(header.trim_end());
        out.push('\n');
        out.push_str("---\n");
        if !record.body.is_empty() {
            out.push_str(&record.body);
            if !record.body.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// Serialize one or more researcher action records.
pub fn format_action_payload(records: &[ActionRecord]) -> Result<String> {
    if records.is_empty() {
        bail!("action payload requires at least one record");
    }
    let mut out = String::new();
    for record in records {
        let header = serde_yaml::to_string(record).context("failed to serialize action record")?;
        out.push_str("---\n");
        out.push_str(header.trim_end());
        out.push('\n');
        out.push_str("---\n");
        if !record.body.is_empty() {
            out.push_str(&record.body);
            if !record.body.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn append_answer_round_trip() {
        let dir = std::env::temp_dir().join(format!("tod-transcript-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("design-interview-2026-08-24-1400.md");
        append_answer(
            &path,
            "q-001",
            "Where should settings live?",
            "Under .local/.config/tod/",
            Some("A"),
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("## q-001"));
        assert!(text.contains("**Selected:** A"));
        assert!(text.contains("**Answer:** Under .local/.config/tod/"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn answer_payload_yaml_shape() {
        let payload = format_answer_payload(&[AnswerRecord {
            id: "q-016".into(),
            option: Some("A".into()),
            body: "Notes".into(),
        }])
        .unwrap();
        assert!(payload.contains("id: q-016"));
        assert!(payload.contains("option: A"));
        assert!(payload.contains("Notes"));
    }
}
