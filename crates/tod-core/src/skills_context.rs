//! The Skills block of an agent's context: the skills the user chose, through
//! the Lifecycle config capability, for the phase the agent is working in,
//! with each one's text put in the block itself.
//!
//! The text goes in rather than the name alone, so the agent spends no turns
//! finding it, and the skill works the same whatever loads skills on the
//! agent's side: a project may keep its phase skills in a grouping folder and
//! mark them `disable-model-invocation` (out of every session's context, and
//! Claude Code's Skill tool then refuses them), and Cursor has no Skill tool.
//!
//! A name is looked up, in order, among tod's own skills (`crate::skills`),
//! then under `.claude/skills/` in the agent's working directory and in each
//! directory above it, nearest first (a project's repository often sits under a
//! workspace that holds the skills for all of them), then under
//! the user's `~/.claude/skills/` where the agent runs (this machine, its dev
//! container, or its sandbox): `<name>/SKILL.md`, or `<group>/<name>/SKILL.md`
//! one grouping folder down. A name with no file anywhere is an error naming
//! the skill and where it was looked for, unless it is one the agent has built
//! in (such as `simplify`), which is passed on by name for the agent's Skill
//! tool, and the block says so.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tod_store::fleet::{FleetStore, Workdir};
use tod_store::lifecycle_config;
use uuid::Uuid;

/// One configured skill, as the agent is given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skill {
    /// Its text was found: tod's own (`path` is `None`), or a `SKILL.md`.
    Text { name: String, path: Option<String>, body: String },
    /// The checkout lacks it but the repository's default branch has it,
    /// read through git: `git_ref` is e.g. `origin/main`, `path` the file in
    /// that branch.
    FromBranch { name: String, git_ref: String, path: String, body: String },
    /// No file has it; the agent loads it by name.
    ByName(String),
}

/// What a missing skill's message says it is, so a caller that only has the
/// error's text (the supervisor, reading a failed turn) can tell a
/// configuration problem from the agent failing.
pub const MISSING_MARKER: &str = "is configured but cannot be found";

/// Whether `error` (any text containing a [`SkillsError::Missing`] message,
/// however it was wrapped) is a missing skill.
pub fn is_missing_skill(error: &str) -> bool {
    error.contains(MISSING_MARKER)
}

/// A skill the lifecycle config names that no place has: a configuration
/// problem for the user, never the agent's failure.
#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    #[error(
        "the skill `{name}` is configured but cannot be found: not among tod's own skills, not under \
         `.claude/skills` in {cwd} or any directory above it, not in the home directory's \
         `.claude/skills`, and not on the repository's default branch ({default_branch}). Add it \
         there, fix its name in the lifecycle config, or remove it"
    )]
    Missing { name: String, cwd: String, default_branch: String },
}

/// The configured skill names for `phase` on `node`, nearest config first
/// (`lifecycle_config::resolve`). Empty when none are configured, including
/// when an explicit empty list turns them off.
pub fn configured(conn: &Connection, node: Uuid, phase: &str) -> Result<Vec<String>> {
    Ok(lifecycle_config::resolve(conn, node, phase)?.map(|r| r.skills).unwrap_or_default())
}

/// The block for `phase` on `node`, for an agent working in `cwd`; empty when
/// no skills are configured. Looking a skill up may run a command in the
/// agent's container or sandbox, so this is never called on the UI thread.
pub fn for_node(fleet: &FleetStore, node: Uuid, phase: &str, cwd: &Workdir) -> Result<String> {
    let names = fleet
        .read(|conn| configured(conn, node, phase))
        .with_context(|| format!("could not read the skills configured for the {phase} phase"))?;
    if names.is_empty() {
        return Ok(String::new());
    }
    let own = crate::skills::SkillsPaths::discover()?;
    let skills = names
        .iter()
        .map(|name| resolve(&own, cwd, name).with_context(|| format!("the {phase} phase's skills")))
        .collect::<Result<Vec<_>>>()?;
    Ok(render_skills(phase, &skills))
}

/// Skills the agent has built in, which have no file to find: passed on by name
/// ([`Skill::ByName`]). Any other name with no file is an error, so a skill the
/// user chose never silently goes missing from the agent's instructions.
const BUILT_IN: &[&str] = &[
    "simplify",
    "code-review",
    "security-review",
    "init",
    "loop",
    "schedule",
    "run",
    "update-config",
    "fewer-permission-prompts",
    "keybindings-help",
    "claude-api",
];

/// Find `name`'s text: tod's own skill, else a `SKILL.md` where the agent
/// runs, else [`Skill::ByName`] for a built-in; any other name is an error.
pub fn resolve(own: &crate::skills::SkillsPaths, cwd: &Workdir, name: &str) -> Result<Skill> {
    if !is_plain_name(name) {
        anyhow::bail!("`{name}` (configured for a lifecycle phase) is not a skill name");
    }
    if own.names()?.iter().any(|n| n == name) {
        return Ok(Skill::Text {
            name: name.to_string(),
            path: None,
            body: own.skill(name)?,
        });
    }
    let found = match cwd {
        Workdir::Host(dir) => find_on_host(dir, home_dir().as_deref(), name)?,
        _ => find_remote(cwd, name)?,
    };
    if let Some((path, text)) = found {
        return Ok(Skill::Text {
            name: name.to_string(),
            path: Some(path),
            body: strip_frontmatter(&text).trim().to_string(),
        });
    }
    if BUILT_IN.contains(&name) {
        return Ok(Skill::ByName(name.to_string()));
    }
    // The checkout may simply be behind the branch the skill was added to.
    let default_branch = match find_on_default_branch(cwd, name) {
        Ok(Ok(found)) => {
            return Ok(Skill::FromBranch {
                name: name.to_string(),
                git_ref: found.git_ref,
                path: found.path,
                body: strip_frontmatter(&found.text).trim().to_string(),
            });
        }
        Ok(Err(searched)) => format!("looked in `{searched}`, which has none"),
        Err(err) => format!("it could not be read: {err:#}"),
    };
    Err(SkillsError::Missing { name: name.to_string(), cwd: cwd.storage(), default_branch }.into())
}

struct OnBranch {
    git_ref: String,
    path: String,
    text: String,
}

/// The skill in the repository's default branch, read through git without
/// checking it out: `Ok(Ok(..))` when found, `Ok(Err(ref))` when the branch
/// was read and has no such skill, `Err` when git or the branch cannot be
/// read.
fn find_on_default_branch(cwd: &Workdir, name: &str) -> Result<std::result::Result<OnBranch, String>> {
    cwd.git(&["rev-parse", "--show-toplevel"])
        .with_context(|| format!("{} is not inside a git repository whose root can be read", cwd.storage()))?;
    let git_ref = default_branch_ref(cwd)?;
    let listing = cwd
        .git(&["ls-tree", "-r", "--name-only", "--full-name", &git_ref, "--", ":/.claude/skills"])
        .with_context(|| format!("could not list `.claude/skills` on {git_ref}"))?;
    let direct = format!(".claude/skills/{name}/SKILL.md");
    let suffix = format!("/{name}/SKILL.md");
    let mut files: Vec<&str> = listing.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    files.sort();
    // Directly under `skills`, else inside a grouping folder (as on disk).
    let path = files
        .iter()
        .find(|f| **f == direct)
        .or_else(|| {
            files.iter().find(|f| {
                f.strip_prefix(".claude/skills/")
                    .and_then(|rest| rest.strip_suffix(&suffix))
                    .is_some_and(|group| !group.is_empty() && !group.contains('/'))
            })
        })
        .map(|f| f.to_string());
    let Some(path) = path else {
        return Ok(Err(git_ref));
    };
    let text = cwd
        .git(&["show", &format!("{git_ref}:{path}")])
        .with_context(|| format!("could not read {path} from {git_ref}"))?;
    Ok(Ok(OnBranch { git_ref, path, text }))
}

/// `origin`'s default branch (`origin/HEAD`), else `origin/main`.
fn default_branch_ref(cwd: &Workdir) -> Result<String> {
    if let Ok(head) = cwd.git(&["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        && !head.is_empty()
    {
        return Ok(head);
    }
    cwd.git(&["rev-parse", "--verify", "--quiet", "origin/main^{commit}"])
        .context("neither `origin/HEAD` nor `origin/main` exists")?;
    Ok("origin/main".to_string())
}

/// A skill name names one folder: nothing that could leave the skills
/// directory or match more than one.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '*', '?', '[', ']', '\n'])
}

/// Where a skill's `SKILL.md` may be under one `skills` directory, in order:
/// directly, then inside each grouping folder.
fn candidates_in(skills: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = vec![skills.join(name).join("SKILL.md")];
    let Ok(entries) = std::fs::read_dir(skills) else {
        return out;
    };
    let mut groups: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    groups.sort();
    out.extend(groups.into_iter().map(|g| g.join(name).join("SKILL.md")));
    out
}

/// [`resolve`]'s lookup on this machine: the `.claude/skills` of the working
/// directory and of each directory above it, nearest first, then `home`'s.
fn find_on_host(cwd: &Path, home: Option<&Path>, name: &str) -> Result<Option<(String, String)>> {
    let mut roots: Vec<PathBuf> = cwd.ancestors().map(|d| d.join(".claude").join("skills")).collect();
    roots.extend(home.map(|h| h.join(".claude").join("skills")));
    for root in roots {
        for path in candidates_in(&root, name) {
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("could not read the skill {}", path.display()))?;
                return Ok(Some((path.display().to_string(), text)));
            }
        }
    }
    Ok(None)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The same lookup inside a container or sandbox, in one command: it prints
/// the path found, then the file; exit 3 when there is none.
const FIND_REMOTE: &str = r#"d="$PWD"
roots=""
while :; do
  roots="$roots
$d/.claude/skills"
  [ "$d" = / ] && break
  d=$(dirname "$d")
done
roots="$roots
$HOME/.claude/skills"
IFS='
'
for root in $roots; do
  for f in "$root/$1/SKILL.md" "$root"/*/"$1"/SKILL.md; do
    if [ -f "$f" ]; then printf '%s\n' "$f"; cat "$f"; exit 0; fi
  done
done
exit 3"#;

fn find_remote(cwd: &Workdir, name: &str) -> Result<Option<(String, String)>> {
    let out = cwd
        .output("sh", &["-c", FIND_REMOTE, "sh", name])
        .with_context(|| format!("could not look for the skill `{name}` in {}", cwd.storage()))?;
    match out.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let (path, body) = text.split_once('\n').unwrap_or((text.as_ref(), ""));
            Ok(Some((path.to_string(), body.to_string())))
        }
        Some(3) => Ok(None),
        _ => anyhow::bail!(
            "could not look for the skill `{name}` in {}: {}",
            cwd.storage(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// `text` without a leading `---` … `---` YAML block.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return text;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return &rest[offset..];
        }
    }
    text
}

/// The block: what the skills are for, then each one's text in order.
pub fn render_skills(phase: &str, skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let names = skills
        .iter()
        .map(|s| match s {
            Skill::Text { name, .. } | Skill::FromBranch { name, .. } | Skill::ByName(name) => format!("`{name}`"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "\n## Skills\n\nThe user chose these skills for the {phase} phase: {names}. Their \
         text is below, in that order: do the phase's work the way they direct. A later one \
         may fill in what an earlier one leaves to the project. Everything else in this \
         message still applies: what the phase must record, and how, is unchanged.\n"
    );
    for skill in skills {
        match skill {
            Skill::Text { name, path, body } => {
                out.push_str(&format!("\n### Skill `{name}`\n\n"));
                if let Some(path) = path {
                    let dir = Path::new(path)
                        .parent()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "From `{path}`. Files it refers to by relative path are in `{dir}`.\n\n"
                    ));
                }
                out.push_str(body.trim());
                out.push('\n');
            }
            Skill::FromBranch { name, git_ref, path, body } => {
                out.push_str(&format!("\n### Skill `{name}`\n\n"));
                out.push_str(&format!(
                    "This checkout does not have this skill, so it is read from `{git_ref}` (`{path}`), \
                     where the repository has it. Files it refers to by relative path are not \
                     available from there: they are not in this checkout, so read them with \
                     `git show {git_ref}:<path>`, or update the branch from `{git_ref}` to have them.\n\n"
                ));
                out.push_str(body.trim());
                out.push('\n');
            }
            Skill::ByName(name) => out.push_str(&format!(
                "\n### Skill `{name}`\n\nNo file has this skill's text, so it is one your agent \
                 provides: use it through your Skill tool. If you have no such skill, say so in \
                 your reply and carry on following what it is meant to do as best you can.\n"
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: &str, path: Option<&str>, body: &str) -> Skill {
        Skill::Text { name: name.into(), path: path.map(Into::into), body: body.into() }
    }

    #[test]
    fn nothing_configured_renders_nothing() {
        assert_eq!(render_skills("review", &[]), "");
    }

    #[test]
    fn each_skills_text_is_in_the_block_in_order() {
        let block = render_skills(
            "pr",
            &[
                text("tod-open-pr", None, "Open the PRs."),
                text("pr-open-step", Some("/w/.claude/skills/tod/pr-open-step/SKILL.md"), "Our repos."),
                Skill::ByName("simplify".into()),
            ],
        );
        assert!(block.contains("pr phase: `tod-open-pr`, `pr-open-step`, `simplify`"), "{block}");
        assert!(block.find("Open the PRs.").unwrap() < block.find("Our repos.").unwrap(), "{block}");
        assert!(
            block.contains("Files it refers to by relative path are in `/w/.claude/skills/tod/pr-open-step`"),
            "{block}"
        );
        assert!(block.contains("### Skill `simplify`") && block.contains("Skill tool"), "{block}");
        assert!(!block.contains("tod-cli skills show"), "{block}");
    }

    #[test]
    fn frontmatter_is_left_out() {
        assert_eq!(
            strip_frontmatter("---\nname: x\ndisable-model-invocation: true\n---\n# X\nbody"),
            "# X\nbody"
        );
        assert_eq!(strip_frontmatter("# X\n---\nbody"), "# X\n---\nbody");
        assert_eq!(strip_frontmatter("---\nunterminated"), "---\nunterminated");
    }

    #[test]
    fn a_skill_is_found_in_the_project_or_a_group_before_home() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let write = |root: &Path, rel: &str, body: &str| {
            let path = root.join(".claude/skills").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        write(project.path(), "tod/ci-fix-step/SKILL.md", "grouped");
        write(project.path(), "steps/up-the-tree/SKILL.md", "workspace root");
        write(project.path(), "direct/SKILL.md", "direct");
        write(home.path(), "direct/SKILL.md", "home direct");
        write(home.path(), "mine/SKILL.md", "home only");
        let find = |name: &str| {
            find_on_host(project.path(), Some(home.path()), name).unwrap().map(|(_, t)| t)
        };
        assert_eq!(find("ci-fix-step").as_deref(), Some("grouped"));
        assert_eq!(find("direct").as_deref(), Some("direct"));
        assert_eq!(find("mine").as_deref(), Some("home only"));
        assert_eq!(find("simplify"), None);
        // The agent runs in a repository under the workspace that holds the skill.
        let inner = project.path().join("kb");
        std::fs::create_dir_all(&inner).unwrap();
        let from_inner = |name: &str| find_on_host(&inner, None, name).unwrap().map(|(_, t)| t);
        assert_eq!(from_inner("up-the-tree").as_deref(), Some("workspace root"));
        assert_eq!(from_inner("mine"), None);
    }

    #[test]
    fn tods_own_skills_come_from_the_bundle_and_others_by_name() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills");
        let own = crate::skills::SkillsPaths::from_root(root).unwrap();
        let empty = tempfile::tempdir().unwrap();
        let cwd = Workdir::host(empty.path());
        let Skill::Text { path: None, body, .. } = resolve(&own, &cwd, "tod-open-pr").unwrap() else {
            panic!("tod-open-pr is tod's own");
        };
        assert!(body.contains("tod-cli phase certify"), "{body}");
        assert!(matches!(resolve(&own, &cwd, "simplify").unwrap(), Skill::ByName(_)));
        let err = format!("{:#}", resolve(&own, &cwd, "not-a-skill-anywhere-xyz").unwrap_err());
        assert!(err.contains("not-a-skill-anywhere-xyz") && err.contains("cannot be found"), "{err}");
        assert!(is_missing_skill(&err), "{err}");
        assert!(resolve(&own, &cwd, "../etc").is_err());
    }

    /// The skills this repository ships for lifecycle phases are in the bundle
    /// the sandboxes get, under the names a lifecycle config uses.
    #[test]
    fn the_pr_skills_ship_in_the_bundle() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills");
        let names = crate::skills::SkillsPaths::from_root(root).unwrap().names().unwrap();
        for name in ["tod-open-pr", "tod-fix-ci"] {
            assert!(names.iter().any(|n| n == name), "{name} missing from {names:?}");
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A branch created before the commit that added a skill still gets it:
    /// from the default branch, named as such.
    #[test]
    fn a_skill_the_checkout_lacks_is_read_from_the_default_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let clone = root.path().join("clone");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "first"]);
        git(root.path(), &["clone", "-q", repo.to_str().unwrap(), clone.to_str().unwrap()]);
        // The skill lands on main after the clone's branch was made.
        git(&clone, &["checkout", "-q", "-b", "task/old"]);
        let skill = repo.join(".claude/skills/steps/x");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: x\n---\n# X from main\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "add skill"]);
        git(&clone, &["fetch", "-q", "origin"]);
        assert!(!clone.join(".claude").exists());

        let own = crate::skills::SkillsPaths::from_root(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills"),
        )
        .unwrap();
        let cwd = Workdir::host(&clone);
        let skill = resolve(&own, &cwd, "x").unwrap();
        let Skill::FromBranch { git_ref, path, body, .. } = &skill else {
            panic!("expected the default branch's copy: {skill:?}");
        };
        assert_eq!(git_ref, "origin/main");
        assert_eq!(path, ".claude/skills/steps/x/SKILL.md");
        assert_eq!(body.trim(), "# X from main");
        let block = render_skills("pr_draft", &[skill]);
        assert!(block.contains("origin/main") && block.contains(".claude/skills/steps/x/SKILL.md"), "{block}");
        assert!(block.contains("relative path"), "{block}");
        assert!(!clone.join(".claude").exists(), "nothing was checked out");

        // A name no branch has is the typed missing error, naming the places.
        let err = resolve(&own, &cwd, "nope-xyz").unwrap_err();
        assert!(matches!(err.downcast_ref::<SkillsError>(), Some(SkillsError::Missing { .. })), "{err:#}");
        let text = format!("{err:#}");
        assert!(text.contains("home directory") && text.contains("origin/main"), "{text}");

        // Outside a repository, the error says the git root could not be read.
        let plain = tempfile::tempdir().unwrap();
        let err = format!("{:#}", resolve(&own, &Workdir::host(plain.path()), "nope-xyz").unwrap_err());
        assert!(err.contains("not inside a git repository") && err.contains("home directory"), "{err}");
    }
}
