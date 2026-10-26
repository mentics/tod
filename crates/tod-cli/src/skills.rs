//! `tod-cli skills` — skills for working with tod itself, read on demand.
//! They are deliberately not in any agent's context
//! (`tod_core::skills`, `assets/skills/`).

use crate::Invocation;
use tod_core::skills::SkillsPaths;

pub(crate) const USAGE: &str = "\
tod-cli skills — skills for working with tod itself, read only when needed

COMMANDS:
    list
    show      <NAME>

`list` prints the README: each skill and when to use it. `show` prints one.
";

pub fn run(inv: Invocation) -> anyhow::Result<String> {
    let mut rest = inv.rest.clone();
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(USAGE.trim_end().to_string());
    }
    // `tod-cli skills` alone is `list`.
    let command = if rest.is_empty() { "list".to_string() } else { rest.remove(0) };
    let paths = SkillsPaths::discover()?;
    match command.as_str() {
        "list" => Ok(paths.readme()?.trim_end().to_string()),
        "show" => {
            let name = rest
                .first()
                .ok_or_else(|| anyhow::anyhow!("`show` needs a skill name (see `tod-cli skills list`)"))?;
            Ok(paths.skill(name)?.trim_end().to_string())
        }
        other => anyhow::bail!("unknown command `{other}`\n\n{}", USAGE.trim_end()),
    }
}
