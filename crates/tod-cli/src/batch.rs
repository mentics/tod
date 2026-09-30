//! `tod-cli batch` — many commands in one call.
//!
//! A script is one ordinary `tod-cli` command per line (without the
//! `--data-root`). The whole script is parsed first, so a malformed one runs
//! nothing and is a single error. Lines then run independently, in order:
//! each that can be applied is, and the reply lists only what was not, so an
//! agent resends the corrected lines alone rather than the script.

use crate::Invocation;
use serde_json::json;
use std::collections::{HashMap, HashSet};

pub(crate) const USAGE: &str = "\
tod-cli batch — run many commands in one call

A script is one tod-cli command per line, written as it would be after
`tod-cli --data-root <PATH>`. Blank lines and lines starting with # are
ignored. The whole script is checked before anything runs; a malformed one
runs nothing.

COMMANDS:
    run    [--file <PATH>]

`run` reads the script from --file, else from stdin. Lines run in order and
independently: each that can be applied is, and the reply is a summary
followed by only the lines that failed (with the line number and the
command's own error) and the output of lines that read. Fix the failed lines
and send just those in a new batch.
Quote a value with \"double\" or 'single' quotes. For multi-line text end a
line with a value of <<TAG and put the text on the following lines, up to a
line holding only TAG.
A line may start `$name =` to bind what it created (a node's slug); later
lines use $name as a whole argument. A line whose $name failed to bind is
reported as skipped.
`batch` and `secrets` cannot be run inside a batch, and a value of - (stdin)
is not allowed; use <<TAG.
";

/// One line of a script, parsed.
#[derive(Debug, PartialEq)]
pub(crate) struct Item {
    /// 1-based line number in the script (where the command starts).
    pub line: usize,
    pub bind: Option<String>,
    pub tokens: Vec<String>,
}

/// A script that could not be parsed: nothing is run.
#[derive(Debug, PartialEq)]
pub(crate) struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The reason a token is refused as a noun inside a batch.
const REFUSED_NOUNS: &[&str] = &["batch", "secrets"];

pub(crate) fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.first().map(String::as_str) == Some("run") {
        rest.remove(0);
    }
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--file" => {
                i += 1;
                file = Some(
                    rest.get(i)
                        .ok_or_else(|| anyhow::anyhow!("--file requires a path"))?
                        .clone(),
                );
            }
            "-" => {}
            other => anyhow::bail!("batch run: unexpected argument `{other}` (see `tod-cli batch --help`)"),
        }
        i += 1;
    }
    let text = match file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("cannot read {path}: {err}"))?,
        None => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            text
        }
    };
    let items = parse_script(&text).map_err(|err| anyhow::anyhow!("malformed batch, nothing ran: {err}"))?;
    if items.is_empty() {
        anyhow::bail!("malformed batch, nothing ran: the script has no commands");
    }

    let root = inv.data_root.to_string_lossy().to_string();
    let report = execute(&items, |tokens| {
        let mut argv = vec!["--data-root".to_string(), root.clone()];
        argv.extend(tokens.iter().cloned());
        crate::run(&argv)
    });
    let failed = report.failed();
    let rendered = if inv.json { report.json() } else { report.text() };
    if failed > 0 {
        // The failures are the reply; an error exit tells the caller so.
        anyhow::bail!("{rendered}");
    }
    Ok(rendered)
}

/// Splits a script into commands, checking everything that can be checked
/// without running: quoting, heredocs, refused nouns, `-` values, variables.
pub(crate) fn parse_script(text: &str) -> Result<Vec<Item>, ParseError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut items = Vec::new();
    let mut bound: HashSet<String> = HashSet::new();
    let mut n = 0;
    while n < lines.len() {
        let line_no = n + 1;
        let raw = lines[n].trim();
        n += 1;
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let fail = |message: String| ParseError { line: line_no, message };
        let mut toks = tokenize(raw).map_err(&fail)?;

        let mut bind = None;
        if let Some(first) = toks.first().map(|t| t.text.clone()) {
            if let Some(name) = first.strip_prefix('$') {
                if toks.get(1).map(|t| t.text.as_str()) == Some("=") {
                    valid_name(name).map_err(&fail)?;
                    bind = Some(name.to_string());
                    toks.drain(..2);
                } else if name.contains('=') {
                    return Err(fail("write a binding as `$name = command`".into()));
                }
            }
        }
        if toks.is_empty() {
            return Err(fail("a binding needs a command after `=`".into()));
        }

        let mut tokens = Vec::with_capacity(toks.len());
        for tok in toks {
            if let Some(tag) = tok.heredoc {
                let mut body = Vec::new();
                loop {
                    let Some(line) = lines.get(n) else {
                        return Err(fail(format!("<<{tag} is never closed (no line holding only {tag})")));
                    };
                    n += 1;
                    if line.trim_end() == tag {
                        break;
                    }
                    body.push(*line);
                }
                tokens.push(body.join("\n"));
            } else {
                tokens.push(tok.text);
            }
        }

        if let Some(noun) = tokens.iter().find(|t| !t.starts_with('-')) {
            if REFUSED_NOUNS.contains(&noun.as_str()) {
                return Err(fail(format!("`{noun}` cannot be run inside a batch")));
            }
        }
        if tokens.iter().any(|t| t == "-") {
            return Err(fail("a value of - reads stdin, which a batch cannot; use <<TAG for long text".into()));
        }
        for token in &tokens {
            if let Some(name) = token.strip_prefix('$') {
                if valid_name(name).is_ok() && !bound.contains(name) {
                    return Err(fail(format!("${name} is used before any line binds it")));
                }
            }
        }
        if let Some(name) = &bind {
            bound.insert(name.clone());
        }
        items.push(Item { line: line_no, bind, tokens });
    }
    Ok(items)
}

fn valid_name(name: &str) -> Result<(), String> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err(format!("`${name}` is not a variable name (letters, digits, _)"))
    }
}

struct Token {
    text: String,
    /// Set for an unquoted `<<TAG`: the text comes from the following lines.
    heredoc: Option<String>,
}

fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut text = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '"' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            None => return Err("unclosed \" quote".into()),
                            Some('"') => break,
                            Some('\\') => match chars.next() {
                                Some(e @ ('"' | '\\')) => text.push(e),
                                Some('n') => text.push('\n'),
                                Some(o) => {
                                    text.push('\\');
                                    text.push(o);
                                }
                                None => return Err("unclosed \" quote".into()),
                            },
                            Some(o) => text.push(o),
                        }
                    }
                }
                '\'' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            None => return Err("unclosed ' quote".into()),
                            Some('\'') => break,
                            Some(o) => text.push(o),
                        }
                    }
                }
                o => text.push(o),
            }
        }
        let heredoc = if !quoted {
            text.strip_prefix("<<").filter(|t| !t.is_empty()).map(str::to_string)
        } else {
            None
        };
        out.push(Token { text, heredoc });
    }
    Ok(out)
}

/// What became of one line.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Applied; `output` is what the command printed.
    Done { output: String, bound: Option<String> },
    Failed(String),
    Skipped(String),
}

#[derive(Debug)]
pub(crate) struct Report {
    pub lines: Vec<(usize, String, Outcome)>,
}

/// A reply longer than this per line is cut, with a marker naming the line.
const MAX_OUTPUT_CHARS: usize = 4000;
const ECHO_CHARS: usize = 70;

/// Runs `items` in order. `run_one` executes one command's arguments.
pub(crate) fn execute(
    items: &[Item],
    mut run_one: impl FnMut(&[String]) -> anyhow::Result<String>,
) -> Report {
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut lines = Vec::with_capacity(items.len());
    for item in items {
        let echo = echo(&item.tokens);
        let mut argv = Vec::with_capacity(item.tokens.len());
        let mut missing = None;
        for token in &item.tokens {
            match token.strip_prefix('$') {
                Some(name) if valid_name(name).is_ok() => match vars.get(name) {
                    Some(value) => argv.push(value.clone()),
                    None => {
                        missing = Some(name.to_string());
                        break;
                    }
                },
                _ => argv.push(token.clone()),
            }
        }
        if let Some(name) = missing {
            lines.push((item.line, echo, Outcome::Skipped(format!("${name} was not bound"))));
            continue;
        }
        match run_one(&argv) {
            Ok(output) => {
                let bound = item.bind.as_ref().and_then(|name| {
                    let value = binding_of(&output)?;
                    vars.insert(name.clone(), value.clone());
                    Some(value)
                });
                if item.bind.is_some() && bound.is_none() {
                    lines.push((
                        item.line,
                        echo,
                        Outcome::Failed("the command ran but printed nothing to bind".into()),
                    ));
                } else {
                    lines.push((item.line, echo, Outcome::Done { output, bound }));
                }
            }
            Err(err) => lines.push((item.line, echo, Outcome::Failed(format!("{err:#}")))),
        }
    }
    Report { lines }
}

/// What `$name` stands for: the id or slug the command acknowledged
/// (`ok <slug>`, or JSON with `id`).
fn binding_of(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(first) {
        let id = value.get("slug").or_else(|| value.get("id"))?.as_str()?;
        return Some(id.to_string());
    }
    first.split_whitespace().last().map(str::to_string)
}

fn echo(tokens: &[String]) -> String {
    let joined = tokens
        .iter()
        .map(|t| if t.contains(char::is_whitespace) { format!("\"{}\"", t.replace('\n', " ")) } else { t.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = joined.chars();
    let head: String = chars.by_ref().take(ECHO_CHARS).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// A command that only acknowledged (`ok …`, or nothing) is not worth its
/// tokens in the reply; anything else is output the agent asked to read.
fn is_ack(output: &str) -> bool {
    let out = output.trim();
    out.is_empty() || (!out.contains('\n') && (out == "ok" || out.starts_with("ok ")))
}

fn capped(line: usize, output: &str) -> String {
    if output.chars().count() <= MAX_OUTPUT_CHARS {
        return output.to_string();
    }
    let head: String = output.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n… line {line}'s output cut at {MAX_OUTPUT_CHARS} characters; run it alone to read all of it")
}

impl Report {
    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.lines.iter().filter(|(_, _, o)| f(o)).count()
    }

    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Failed(_)))
    }

    fn summary(&self) -> String {
        let applied = self.count(|o| matches!(o, Outcome::Done { .. }));
        let skipped = self.count(|o| matches!(o, Outcome::Skipped(_)));
        let mut s = format!("batch: {applied} applied, {} failed", self.failed());
        if skipped > 0 {
            s.push_str(&format!(", {skipped} skipped"));
        }
        s
    }

    pub fn text(&self) -> String {
        let mut out = vec![self.summary()];
        for (line, echo, outcome) in &self.lines {
            match outcome {
                Outcome::Done { output, bound } => {
                    if let (Some(value), true) = (bound, is_ack(output)) {
                        out.push(format!("[{line}] {echo}\n  bound {value}"));
                    } else if !is_ack(output) {
                        out.push(format!("[{line}] {echo}\n{}", capped(*line, output.trim_end())));
                    }
                }
                Outcome::Failed(message) => out.push(format!("[{line}] FAILED {echo}\n  {message}")),
                Outcome::Skipped(why) => out.push(format!("[{line}] SKIPPED {echo}\n  {why}")),
            }
        }
        out.join("\n")
    }

    pub fn json(&self) -> String {
        let results: Vec<_> = self
            .lines
            .iter()
            .map(|(line, echo, outcome)| match outcome {
                Outcome::Done { output, bound } => json!({
                    "line": line, "cmd": echo, "ok": true,
                    "output": capped(*line, output.trim_end()), "bound": bound,
                }),
                Outcome::Failed(message) => json!({ "line": line, "cmd": echo, "ok": false, "error": message }),
                Outcome::Skipped(why) => json!({ "line": line, "cmd": echo, "ok": false, "skipped": why }),
            })
            .collect();
        json!({ "summary": self.summary(), "results": results }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(items: &[Item]) -> Vec<Vec<&str>> {
        items.iter().map(|i| i.tokens.iter().map(String::as_str).collect()).collect()
    }

    #[test]
    fn lines_split_with_quotes_comments_and_blanks() {
        let items = parse_script(
            "# a note\n\nnode rename foo --title \"Two words\"\nobligations add --body 'it\\'s' --kind req\n",
        );
        // `\'` inside single quotes is literal, so this line is unterminated.
        assert!(items.is_err());
        let items = parse_script("# a note\n\nnode rename foo --title \"Two \\\"words\\\"\"\n").unwrap();
        assert_eq!(cmds(&items), vec![vec!["node", "rename", "foo", "--title", "Two \"words\""]]);
        assert_eq!(items[0].line, 3);
    }

    #[test]
    fn a_heredoc_supplies_a_multi_line_value() {
        let items = parse_script("obligations add --node n --kind req --body <<EOT\nline one\n\nline $two\nEOT\nnode list --parent n\n").unwrap();
        assert_eq!(items[0].tokens.last().unwrap(), "line one\n\nline $two");
        assert_eq!(items[1].line, 6);
    }

    #[test]
    fn a_malformed_script_is_one_error_naming_the_line() {
        assert_eq!(parse_script("node list\nnode rename x --title \"oops\n").unwrap_err().line, 2);
        assert!(parse_script("a --body <<EOT\nnever closed\n").unwrap_err().message.contains("never closed"));
        assert!(parse_script("batch run").is_err());
        assert!(parse_script("secrets run -- x").is_err());
        assert!(parse_script("obligations add --body -").is_err());
        assert!(parse_script("node list --parent $a").unwrap_err().message.contains("before any line binds"));
        assert!(parse_script("$a=node create --title x").is_err());
    }

    #[test]
    fn a_dollar_in_text_is_not_a_variable() {
        // Only a whole argument that is `$name` is one; prices are prices.
        assert!(parse_script("obligations add --body \"costs $5 or $a\" --kind req").is_ok());
    }

    #[test]
    fn a_failure_does_not_stop_the_rest_and_only_failures_are_reported() {
        let items = parse_script("plan add one\nplan add bad\nplan add three\n").unwrap();
        let report = execute(&items, |argv| {
            if argv.iter().any(|a| a == "bad") { anyhow::bail!("no such step") } else { Ok("ok x".into()) }
        });
        assert_eq!(report.failed(), 1);
        let text = report.text();
        assert!(text.starts_with("batch: 2 applied, 1 failed"), "{text}");
        assert!(text.contains("[2] FAILED plan add bad\n  no such step"), "{text}");
        assert!(!text.contains("[1]") && !text.contains("[3]"), "{text}");
    }

    #[test]
    fn a_binding_feeds_later_lines_and_a_failed_one_skips_them() {
        let items = parse_script("$a = node create --title A --parent root\nnode create --title B --parent $a\n$c = node create --title C\nnode rename $c --title D\n").unwrap();
        let mut seen = Vec::new();
        let report = execute(&items, |argv| {
            seen.push(argv.join(" "));
            if argv.contains(&"C".to_string()) { anyhow::bail!("boom") } else { Ok("ok a-slug".into()) }
        });
        assert!(seen.contains(&"node create --title B --parent a-slug".to_string()), "{seen:?}");
        assert_eq!(report.failed(), 1);
        let text = report.text();
        assert!(text.starts_with("batch: 2 applied, 1 failed, 1 skipped"), "{text}");
        assert!(text.contains("[4] SKIPPED"), "{text}");
    }

    #[test]
    fn reads_print_their_output_tagged_with_their_line() {
        let items = parse_script("node list --parent p\nplan add x\n").unwrap();
        let report = execute(&items, |argv| {
            Ok(if argv[0] == "node" { "alpha\nbeta".into() } else { "ok".into() })
        });
        let text = report.text();
        assert!(text.contains("[1] node list --parent p\nalpha\nbeta"), "{text}");
        assert!(!text.contains("[2]"), "{text}");
    }

    #[test]
    fn a_huge_read_is_cut_with_a_marker_naming_its_line() {
        let items = parse_script("node list\n").unwrap();
        let report = execute(&items, |_| Ok("x\n".repeat(5000)));
        assert!(report.text().contains("line 1's output cut"));
    }

    #[test]
    fn json_reports_each_line() {
        let items = parse_script("plan add bad\n").unwrap();
        let report = execute(&items, |_| anyhow::bail!("nope"));
        let value: serde_json::Value = serde_json::from_str(&report.json()).unwrap();
        assert_eq!(value["results"][0]["ok"], false);
        assert_eq!(value["results"][0]["error"], "nope");
    }
}
