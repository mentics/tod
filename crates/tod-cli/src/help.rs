//! `tod-cli help <WORDS>` — find the commands that do something, across every
//! noun, without reading each noun's help in turn.

/// `help` alone is the top-level usage, `help <NOUN>` that noun's, and
/// anything else a search.
pub fn run(words: &[String], top: &str) -> String {
    let nouns = crate::doc_sync::nouns();
    if words.is_empty() {
        return top.trim_end().to_string();
    }
    if let [one] = words {
        if let Some((_, usage)) = nouns.iter().find(|(noun, _)| noun == one) {
            return usage.trim_end().to_string();
        }
    }
    let words: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let mut out = Vec::new();
    for (noun, usage) in &nouns {
        let text = format!("{noun}\n{usage}").to_lowercase();
        if !words.iter().all(|w| text.contains(w.as_str())) {
            continue;
        }
        let summary = usage.lines().next().unwrap_or_default();
        let commands = command_lines(usage);
        let hits: Vec<&str> = commands
            .iter()
            .copied()
            .filter(|line| words.iter().any(|w| line.to_lowercase().contains(w.as_str())))
            .collect();
        out.push(summary.to_string());
        for line in if hits.is_empty() { commands } else { hits } {
            out.push(format!("    tod-cli {noun} {}", line.trim()));
        }
    }
    if out.is_empty() {
        return format!(
            "nothing matches `{}`; try fewer or other words, or `tod-cli --help` for every noun",
            words.join(" ")
        );
    }
    out.push("Run `tod-cli <NOUN> --help` for a noun's options and rules.".to_string());
    out.join("\n")
}

/// The lines under `COMMANDS:`, continuation lines included, each prefixed
/// with its verb when it is a continuation.
fn command_lines(usage: &str) -> Vec<&str> {
    usage
        .lines()
        .skip_while(|l| !l.starts_with("COMMANDS:"))
        .skip(1)
        .take_while(|l| l.starts_with("    "))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_for_lifecycle_finds_how_to_enable_it() {
        let found = run(&["lifecycle".into()], "");
        assert!(found.contains("tod-cli capabilities enable"), "{found}");
    }

    #[test]
    fn a_noun_name_shows_its_usage() {
        assert_eq!(run(&["plan".into()], ""), crate::plan::USAGE.trim_end());
    }
}
