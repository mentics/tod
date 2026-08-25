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
    pub options: Vec<McOption>,
    pub body: String,
    pub short_label: String,
}

#[derive(Debug, Deserialize)]
struct QueueFrontMatter {
    id: String,
    #[serde(default)]
    created: Option<String>,
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
    let created = meta
        .created
        .as_deref()
        .map(parse_timestamp)
        .transpose()?;
    let options = meta
        .options
        .into_iter()
        .map(|o| McOption {
            key: o.key,
            label: o.label,
        })
        .collect();
    let body = body.trim().to_string();
    let short_label = short_label_from_body(&body);
    Ok(QueueQuestion {
        id: meta.id,
        path: path.to_path_buf(),
        created,
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

fn split_front_matter(contents: &str) -> Result<(&str, &str)> {
    let trimmed = contents.trim_start();
    if !trimmed.starts_with("---") {
        bail!("queue file missing YAML front matter");
    }
    let after_open = trimmed
        .strip_prefix("---")
        .context("queue front matter open delimiter missing")?
        .trim_start_matches(['\r', '\n']);
    let close = after_open.find("\n---").context("queue front matter close delimiter missing")?;
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

fn short_label_from_body(body: &str) -> String {
    body.lines()
        .find(|line| {
            let t = line.trim();
            !t.is_empty() && !t.starts_with("**Recommend:")
        })
        .unwrap_or(body)
        .chars()
        .take(72)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_front_matter_and_body() {
        let contents = r#"---
id: q-001
created: 2026-08-22T14:00:00-07:00
options:
  - key: A
    label: First option
  - key: B
    label: Second option
---
Where should durable interview transcripts live?

**Recommend:** A
Which do you want?
"#;
        let q = parse_queue_contents(Path::new("q-001.md"), contents).unwrap();
        assert_eq!(q.id, "q-001");
        assert_eq!(q.options.len(), 2);
        assert_eq!(q.options[0].key, "A");
        assert!(q.body.contains("Where should durable"));
    }
}
