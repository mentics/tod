//! Minimal `--flag value` / positional argument parsing shared by nouns.

use std::collections::HashMap;
use uuid::Uuid;

/// Flags that never take a value.
const SWITCHES: &[&str] = &["--before", "--append", "--inherited"];

#[derive(Default)]
pub struct Args {
    pub positional: Vec<String>,
    flags: HashMap<String, String>,
    switches: Vec<String>,
}

impl Args {
    pub fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut out = Args::default();
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
                out.flags.insert(arg.clone(), value.clone());
            } else {
                out.positional.push(arg.clone());
            }
            i += 1;
        }
        Ok(out)
    }

    pub fn get(&self, flag: &str) -> Option<&str> {
        self.flags.get(flag).map(String::as_str)
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
    fn missing_values_and_bad_uuids_are_errors() {
        assert!(parse(&["--body"]).is_err());
        assert!(parse(&["--node", "not-a-uuid"]).unwrap().node().is_err());
        assert!(parse(&[]).unwrap().require("--kind").is_err());
    }
}
