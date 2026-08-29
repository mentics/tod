use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McOption {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueQuestion {
    pub id: String,
    pub path: PathBuf,
    pub created: Option<DateTime<Utc>>,
    pub layer: Option<String>,
    pub kind: Option<String>,
    pub covers: Vec<String>,
    pub context: Option<String>,
    pub question: Option<String>,
    pub recommend: Option<String>,
    pub proposed_text: Option<String>,
    pub options: Vec<McOption>,
    /// Raw markdown body after front matter (legacy / empty for structured files).
    pub body: String,
    pub short_label: String,
}

impl QueueQuestion {
    /// Compose transcript / fallback prose from structured fields (or legacy body).
    pub fn display_body(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(context) = self
            .context
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(context.to_string());
        }
        if let Some(question) = self
            .question
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(question.to_string());
        }
        if parts.is_empty() {
            let legacy = strip_mc_option_lines(&self.body, &self.options);
            let (legacy, _) = split_recommend_from_body(&legacy);
            if !legacy.trim().is_empty() {
                parts.push(legacy.trim().to_string());
            }
        }
        if let Some(recommend) = self
            .recommend
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(format!("**Recommend:** {recommend}"));
        }
        parts.join("\n\n")
    }
}

#[derive(Debug, Deserialize)]
struct QueueFrontMatter {
    id: String,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    covers: Vec<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    recommend: Option<String>,
    #[serde(default)]
    proposed_text: Option<String>,
    #[serde(default)]
    options: Vec<McOptionYaml>,
}

#[derive(Debug, Deserialize)]
struct McOptionYaml {
    key: String,
    label: String,
}

/// Parse a queue file: YAML front matter + markdown body.
pub fn parse_queue_file(path: &Path) -> Result<QueueQuestion> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read queue file {}", path.display()))?;
    parse_queue_contents(path, &contents)
}

pub fn parse_queue_contents(path: &Path, contents: &str) -> Result<QueueQuestion> {
    let (front_matter, body) = split_front_matter(contents)?;
    let meta: QueueFrontMatter =
        serde_yaml::from_str(front_matter).context("failed to parse queue YAML front matter")?;
    let created = meta.created.as_deref().map(parse_timestamp).transpose()?;
    let options = meta
        .options
        .into_iter()
        .map(|o| McOption {
            key: o.key,
            label: o.label,
        })
        .collect::<Vec<_>>();
    let body = body.trim().to_string();
    let context = nonempty_opt(meta.context);
    let question = nonempty_opt(meta.question);
    let proposed_text = nonempty_opt(meta.proposed_text);
    let mut recommend = nonempty_opt(meta.recommend);
    if recommend.is_none() {
        let (_, from_body) = split_recommend_from_body(&body);
        recommend = from_body;
    }
    let short_label = short_label_for(&question, &context, &body);
    Ok(QueueQuestion {
        id: meta.id,
        path: path.to_path_buf(),
        created,
        layer: nonempty_opt(meta.layer),
        kind: nonempty_opt(meta.kind),
        covers: meta.covers,
        context,
        question,
        recommend,
        proposed_text,
        options,
        body,
        short_label,
    })
}

pub fn load_queue_dir(dir: &Path) -> Result<Vec<QueueQuestion>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut questions = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("failed to read queue dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        questions.push(parse_queue_file(&path)?);
    }
    questions.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(questions)
}

fn nonempty_opt(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let t = s.trim().to_string();
        if t.is_empty() { None } else { Some(t) }
    })
}

fn split_front_matter(contents: &str) -> Result<(&str, &str)> {
    let trimmed = contents.trim_start();
    if !trimmed.starts_with("---") {
        bail!("queue file missing YAML front matter");
    }
    let after_open = trimmed
        .strip_prefix("---")
        .context("queue front matter open delimiter missing")?
        .trim_start_matches(['\r', '\n']);
    let close = after_open
        .find("\n---")
        .context("queue front matter close delimiter missing")?;
    let yaml = &after_open[..close];
    let body = after_open[close + 4..].trim_start_matches(['\r', '\n']);
    Ok((yaml, body))
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f%z") {
        return Ok(dt.with_timezone(&Utc));
    }
    anyhow::bail!("unsupported timestamp format: {value}")
}

fn short_label_for(question: &Option<String>, context: &Option<String>, body: &str) -> String {
    if let Some(q) = question.as_deref() {
        return truncate_label(q);
    }
    if let Some(c) = context.as_deref() {
        if let Some(line) = c.lines().find(|l| !l.trim().is_empty()) {
            return truncate_label(line);
        }
    }
    let (legacy, _) = split_recommend_from_body(body);
    truncate_label(
        legacy
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(body),
    )
}

fn truncate_label(text: &str) -> String {
    text.chars().take(72).collect()
}

/// Split a legacy body into (body_without_recommend, recommend_text).
pub fn split_recommend_from_body(body: &str) -> (String, Option<String>) {
    let mut recommend = None;
    let mut kept = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = strip_recommend_prefix(trimmed) {
            recommend = Some(rest.to_string());
        } else {
            kept.push(line);
        }
    }
    let cleaned = kept.join("\n").trim().to_string();
    (cleaned, recommend)
}

fn strip_recommend_prefix(line: &str) -> Option<&str> {
    const PREFIXES: &[&str] = &[
        "**Recommend:**",
        "**Recommend**:",
        "Recommend:",
        "Recommend：",
    ];
    for prefix in PREFIXES {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim());
        }
    }
    None
}

/// Drop legacy body lines that restate MC options already in front matter.
pub fn strip_mc_option_lines(body: &str, options: &[McOption]) -> String {
    if options.is_empty() {
        return body.to_string();
    }
    let keys: Vec<&str> = options.iter().map(|o| o.key.as_str()).collect();
    body.lines()
        .filter(|line| !line_looks_like_mc_option(line.trim(), &keys))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn line_looks_like_mc_option(line: &str, keys: &[&str]) -> bool {
    for key in keys {
        let patterns = [
            format!("**{key})**"),
            format!("**{key}.**"),
            format!("**{key})"),
            format!("{key})"),
            format!("{key}."),
        ];
        for pattern in &patterns {
            if line.starts_with(pattern.as_str()) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_front_matter() {
        let contents = r#"---
id: q-001
created: 2026-08-22T14:00:00-07:00
layer: task
kind: decision
covers: [primary-store]
context: |
  For fleet-state persistence we need one primary store.
question: Which primary on-disk store should we lock?
recommend: 1 — SQLite fits local tooling
proposed_text: |
  Fleet-state persistence uses SQLite.
options:
  - key: "1"
    label: Accept as written
  - key: "2"
    label: Use JSON files instead
---
"#;
        let q = parse_queue_contents(Path::new("q-001.md"), contents).unwrap();
        assert_eq!(q.id, "q-001");
        assert_eq!(q.layer.as_deref(), Some("task"));
        assert_eq!(q.kind.as_deref(), Some("decision"));
        assert_eq!(q.covers, vec!["primary-store"]);
        assert!(q.context.as_deref().unwrap().contains("fleet-state"));
        assert_eq!(
            q.question.as_deref(),
            Some("Which primary on-disk store should we lock?")
        );
        assert_eq!(
            q.recommend.as_deref(),
            Some("1 — SQLite fits local tooling")
        );
        assert!(q.proposed_text.as_deref().unwrap().contains("SQLite"));
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].key, "1");
        assert!(q.body.trim().is_empty());
        assert!(q.short_label.contains("primary on-disk"));
        assert!(q.display_body().contains("**Recommend:** 1 —"));
    }

    #[test]
    fn parses_legacy_body_and_extracts_recommend() {
        let contents = r#"---
id: q-001
created: 2026-08-22T14:00:00-07:00
options:
  - key: "1"
    label: First option
  - key: "2"
    label: Second option
---
Where should durable interview transcripts live?

**1)** Project history only
**2)** Shared doc plus history

**Recommend:** 1
Which do you want?
"#;
        let q = parse_queue_contents(Path::new("q-001.md"), contents).unwrap();
        assert_eq!(q.id, "q-001");
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.recommend.as_deref(), Some("1"));
        let display = strip_mc_option_lines(&q.body, &q.options);
        let (display, _) = split_recommend_from_body(&display);
        assert!(display.contains("Where should durable"));
        assert!(!display.contains("**1)**"));
        assert!(!display.contains("**Recommend:**"));
    }
}
