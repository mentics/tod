//! Minimal `--flag value` / positional argument parsing shared by nouns.

use std::collections::HashMap;
use uuid::Uuid;

/// Flags that never take a value.
const SWITCHES: &[&str] = &["--before", "--append", "--inherited"];

#[derive(Default)]
pub struct Args {
    pub positional: Vec<String>,
    flags: HashMap<String, String>,
    /// Every flag value in order, repeats included, for [`Args::get_all`].
    all: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Args {
    pub fn parse(args: &[String]) -> anyhow::Result<Self> {
        Self::parse_with_stdin(args, || {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            Ok(text)
        })
    }

    /// [`Args::parse`], with `read_stdin` supplying the text of the one flag
    /// whose value is `-` (`--body -` with a heredoc, for long or multi-line text).
    pub fn parse_with_stdin(
        args: &[String],
        read_stdin: impl FnOnce() -> std::io::Result<String>,
    ) -> anyhow::Result<Self> {
        let mut out = Args::default();
        let mut stdin_flag: Option<&String> = None;
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if SWITCHES.contains(&arg.as_str()) {
                out.switches.push(arg.clone());
            } else if arg.starts_with("--") {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("{arg} requires a value"))?;
                if value == "-" {
                    if let Some(first) = stdin_flag {
                        anyhow::bail!("only one flag can read stdin, but {first} and {arg} are both `-`");
                    }
                    stdin_flag = Some(arg);
                }
                out.flags.insert(arg.clone(), value.clone());
                out.all.push((arg.clone(), value.clone()));
            } else {
                out.positional.push(arg.clone());
            }
            i += 1;
        }
        if let Some(flag) = stdin_flag {
            let text = read_stdin()?;
            let text = text.trim_end_matches(['\r', '\n']);
            if text.trim().is_empty() {
                anyhow::bail!("{flag} - read nothing from stdin");
            }
            out.flags.insert(flag.clone(), text.to_string());
            for (name, value) in &mut out.all {
                if name == flag && value == "-" {
                    *value = text.to_string();
                }
            }
        }
        Ok(out)
    }

    pub fn get(&self, flag: &str) -> Option<&str> {
        self.flags.get(flag).map(String::as_str)
    }

    /// Every value a repeatable flag was given, in order. [`Args::get`] sees
    /// only the last.
    pub fn get_all(&self, flag: &str) -> Vec<&str> {
        self.all
            .iter()
            .filter(|(name, _)| name == flag)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn require(&self, flag: &str) -> anyhow::Result<&str> {
        self.get(flag)
            .ok_or_else(|| anyhow::anyhow!("{flag} is required"))
    }

    pub fn has(&self, switch: &str) -> bool {
        self.switches.iter().any(|s| s == switch)
    }

    pub fn uuid(&self, flag: &str) -> anyhow::Result<Option<Uuid>> {
        self.get(flag)
            .map(|raw| {
                Uuid::parse_str(raw)
                    .map_err(|_| anyhow::anyhow!("{flag}: `{raw}` is not a valid UUID"))
            })
            .transpose()
    }

    pub fn node(&self) -> anyhow::Result<Uuid> {
        self.uuid("--node")?
            .ok_or_else(|| anyhow::anyhow!("--node <UUID> is required"))
    }

    pub fn target(&self, what: &str) -> anyhow::Result<&str> {
        self.positional
            .first()
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("{what} is required"))
    }

    /// `q-14`, `m-3`, or a bare number.
    pub fn seq(&self, prefix: &str) -> anyhow::Result<i64> {
        let raw = self.target(&format!("a {prefix}-<n> id"))?;
        raw.trim()
            .strip_prefix(&format!("{prefix}-"))
            .unwrap_or(raw.trim())
            .parse()
            .map_err(|_| anyhow::anyhow!("`{raw}` is not a {prefix}-<n> id"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Args> {
        Args::parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_switches_and_positionals() {
        let args =
            parse(&["q-14", "--reason", "stale", "--append", "--body", "two words"]).unwrap();
        assert_eq!(args.positional, ["q-14"]);
        assert_eq!(args.get("--reason"), Some("stale"));
        assert_eq!(args.get("--body"), Some("two words"));
        assert!(args.has("--append"));
        assert!(!args.has("--before"));
        assert_eq!(args.seq("q").unwrap(), 14);
    }

    #[test]
    fn ids_accept_the_prefix_or_a_bare_number() {
        assert_eq!(parse(&["m-3"]).unwrap().seq("m").unwrap(), 3);
        assert_eq!(parse(&["7"]).unwrap().seq("q").unwrap(), 7);
        assert!(parse(&["x-2"]).unwrap().seq("q").is_err());
        assert!(parse(&[]).unwrap().seq("q").is_err());
    }

    #[test]
    fn a_dash_value_reads_its_text_from_stdin() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let args = Args::parse_with_stdin(&argv(&["--kind", "req", "--body", "-"]), || {
            Ok("Line one.\nLine two.\n".into())
        })
        .unwrap();
        assert_eq!(args.get("--body"), Some("Line one.\nLine two."));
        assert_eq!(args.get("--kind"), Some("req"));
        assert!(Args::parse_with_stdin(&argv(&["--body", "-"]), || Ok("\n".into())).is_err());
        assert!(
            Args::parse_with_stdin(&argv(&["--body", "-", "--why", "-"]), || Ok("x".into())).is_err()
        );
    }

    #[test]
    fn missing_values_and_bad_uuids_are_errors() {
        assert!(parse(&["--body"]).is_err());
        assert!(parse(&["--node", "not-a-uuid"]).unwrap().node().is_err());
        assert!(parse(&[]).unwrap().require("--kind").is_err());
    }
}
