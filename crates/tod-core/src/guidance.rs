//! Guidance selection (`doc/lifecycle/find-it-first.md`, section 12): which
//! of the project's own instructions (its skills, the files every reviewer
//! reads), tod's review lenses and the phases' checks a task's change needs.
//! It is picked before the work, again when the task enters review, and once
//! more after a fix whose change reached new areas; the selection
//! (`tod_store::guidance`) is inlined, text and all, into every implement,
//! verify, review and fix opening ([`render_selection`]).
//!
//! The app matches first, with no model: a skill whose front matter says
//! `applies_to: { paths: [...], terms: [...] }` is selected when a changed file
//! matches one of its globs or the change mentions one of its terms, and is
//! never offered to the selector. The rest is the selector's
//! (`ProtocolKind::Guidance`, one cheap session: haiku at low effort unless the
//! lifecycle config's `guidance` key says otherwise): given the change summary,
//! the candidates, the project's routing table and the lenses, it records each
//! match with a one-line reason and each lens that does not apply. It reads no
//! skill.

use crate::context_recipes::{GUIDANCE, build_message};
use crate::dynamic::{DynamicContext, NodeSelection};
use crate::media::MediaPaths;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::{FleetStore, Workdir, worktree};
use tod_store::guidance::{
    self as store, AppMatch, Candidates, Guidance, KIND_CHECK, KIND_FILE, KIND_LENS, KIND_SKILL, NewRun, Run,
    STAGE_ACTUAL, STAGE_GROWN, STAGE_PREDICTED,
};
use tod_store::lifecycle_config;
use tod_store::outline::EXTRA_CONTENT_DETAILS;
use tod_store::outline::repos::{NodeRepo, ObligationRepo};
use tod_store::phase::{PHASE_CERTIFY, PhaseRepo};
use uuid::Uuid;

/// What the selector session is first told.
pub const STARTER: &str = "Select this task's guidance from the candidates in your context and record each \
choice with its reason, and every lens as applying or not.";

/// Where a skill's own triggers live in its `SKILL.md` front matter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Triggers {
    /// Globs over the changed files' paths (`*` matches across `/`).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Words or phrases the change's text is matched against, ignoring case.
    #[serde(default)]
    pub terms: Vec<String>,
}

impl Triggers {
    pub fn is_empty(&self) -> bool {
        self.paths.iter().all(|p| p.trim().is_empty()) && self.terms.iter().all(|t| t.trim().is_empty())
    }
}

/// One of the project's skills, as the selector is offered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSkill {
    pub name: String,
    pub path: String,
    /// The front matter's `description`, on one line; empty when it has none.
    pub description: String,
    /// The front matter's `applies_to`; such a skill is decided by the app.
    pub triggers: Option<Triggers>,
}

#[derive(Debug, Default, Deserialize)]
struct FrontMatter {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    applies_to: Option<Triggers>,
}

/// The YAML between a leading `---` line and the next one.
fn front_matter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    None
}

fn one_line(text: &str, cap: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > cap {
        format!("{}…", joined.chars().take(cap).collect::<String>())
    } else {
        joined
    }
}

/// A skill's description and triggers from its `SKILL.md`. Front matter that
/// is not YAML still gives its `description:` line (agents read it as loosely),
/// unless it declares `applies_to`: triggers that cannot be read are an error
/// naming the file, never silently no triggers.
pub fn parse_skill(name: &str, path: &str, text: &str) -> Result<ProjectSkill> {
    let mut skill = ProjectSkill {
        name: name.to_string(),
        path: path.to_string(),
        description: String::new(),
        triggers: None,
    };
    let Some(yaml) = front_matter(text) else {
        return Ok(skill);
    };
    match serde_yaml::from_str::<FrontMatter>(yaml) {
        Ok(front) => {
            skill.description = one_line(front.description.as_deref().unwrap_or(""), 300);
            skill.triggers = front.applies_to.filter(|t| !t.is_empty());
        }
        Err(err) => {
            if yaml.contains("applies_to") {
                bail!(
                    "the front matter of {path} is not valid YAML, so its `applies_to` triggers cannot be \
                     read: {err}. Fix the front matter, or remove `applies_to`"
                );
            }
            skill.description = yaml
                .lines()
                .find_map(|l| l.trim().strip_prefix("description:"))
                .map(|d| one_line(d.trim().trim_matches('"'), 300))
                .unwrap_or_default();
        }
    }
    Ok(skill)
}

/// What the project offers where the agent works: its skills (nearest
/// `.claude/skills` first, a name's first one wins), its `CLAUDE.md` files
/// (nearest first), and which of the always-read candidates exist.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    pub skills: Vec<ProjectSkill>,
    /// `(path, text)`.
    pub docs: Vec<(String, String)>,
    /// The asked-for files that exist, relative to the working directory.
    pub files: Vec<String>,
}

fn add_skill(survey: &mut Survey, path: &str, text: &str) -> Result<()> {
    let Some(name) = Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
    else {
        return Ok(());
    };
    if survey.skills.iter().any(|s| s.name == name) {
        return Ok(());
    }
    survey.skills.push(parse_skill(&name, path, text)?);
    Ok(())
}

/// Look around `cwd` for the project's skills and instructions, and for which
/// of `wanted` (relative files) exist. The home directory's own are not the
/// project's and are left out. Runs a command in a container or sandbox, so
/// never on the UI thread.
pub fn survey(cwd: &Workdir, wanted: &[String]) -> Result<Survey> {
    match cwd {
        Workdir::Host(dir) => survey_host(dir, home_dir().as_deref(), wanted),
        _ => survey_remote(cwd, wanted),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs
}

fn survey_host(cwd: &Path, home: Option<&Path>, wanted: &[String]) -> Result<Survey> {
    let mut survey = Survey::default();
    let read = |path: &Path| std::fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()));
    for dir in cwd.ancestors().filter(|d| Some(*d) != home) {
        let skills = dir.join(".claude").join("skills");
        let mut found = Vec::new();
        for entry in sorted_dirs(&skills) {
            found.push(entry.join("SKILL.md"));
        }
        for group in sorted_dirs(&skills) {
            for entry in sorted_dirs(&group) {
                found.push(entry.join("SKILL.md"));
            }
        }
        for path in found.into_iter().filter(|p| p.is_file()) {
            add_skill(&mut survey, &path.display().to_string(), &read(&path)?)?;
        }
        for doc in [dir.join("CLAUDE.md"), dir.join(".claude").join("CLAUDE.md")] {
            if doc.is_file() {
                survey.docs.push((doc.display().to_string(), read(&doc)?));
            }
        }
    }
    survey.files = wanted.iter().filter(|rel| cwd.join(rel).is_file()).cloned().collect();
    Ok(survey)
}

/// [`survey_host`] in one command: a record per file found, each opened by
/// the record separator (`\x1e`) and `skill <path>`, `doc <path>` or
/// `file <relative path>`, then the file's text (none for `file`).
const SURVEY_REMOTE: &str = r#"d="$PWD"
dirs=""
while :; do
  [ "$d" != "$HOME" ] && dirs="$dirs
$d"
  [ "$d" = / ] && break
  d=$(dirname "$d")
done
IFS='
'
for dir in $dirs; do
  for f in "$dir"/.claude/skills/*/SKILL.md "$dir"/.claude/skills/*/*/SKILL.md; do
    if [ -f "$f" ]; then printf '\036skill %s\n' "$f"; cat "$f"; fi
  done
  for f in "$dir/CLAUDE.md" "$dir/.claude/CLAUDE.md"; do
    if [ -f "$f" ]; then printf '\036doc %s\n' "$f"; cat "$f"; fi
  done
done
for rel in "$@"; do
  if [ -f "$rel" ]; then printf '\036file %s\n' "$rel"; fi
done
exit 0"#;

fn survey_remote(cwd: &Workdir, wanted: &[String]) -> Result<Survey> {
    let mut args = vec!["-c", SURVEY_REMOTE, "sh"];
    args.extend(wanted.iter().map(String::as_str));
    let out = cwd
        .output("sh", &args)
        .with_context(|| format!("could not look for the project's guidance in {}", cwd.storage()))?;
    if !out.status.success() {
        bail!(
            "could not look for the project's guidance in {}: {}",
            cwd.storage(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_survey(&String::from_utf8_lossy(&out.stdout))
}

fn parse_survey(text: &str) -> Result<Survey> {
    let mut survey = Survey::default();
    for record in text.split('\x1e').filter(|r| !r.is_empty()) {
        let (head, body) = record.split_once('\n').unwrap_or((record, ""));
        let Some((kind, path)) = head.split_once(' ') else {
            continue;
        };
        match kind {
            "skill" => add_skill(&mut survey, path, body)?,
            "doc" => survey.docs.push((path.to_string(), body.to_string())),
            "file" => survey.files.push(path.to_string()),
            _ => {}
        }
    }
    Ok(survey)
}

/// The lines of the project's `CLAUDE.md` files that route work to a skill:
/// table rows and list items that name one, under the file they came from.
pub fn routing_table(survey: &Survey) -> String {
    const CAP: usize = 80;
    let mut out = String::new();
    let mut taken = 0;
    for (path, text) in &survey.docs {
        let rows: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('|') || l.starts_with("- ") || l.starts_with("* "))
            .filter(|l| survey.skills.iter().any(|s| names_skill(l, &s.name)))
            .collect();
        if rows.is_empty() || taken >= CAP {
            continue;
        }
        out.push_str(&format!("From `{path}`:\n\n"));
        for row in rows.into_iter().take(CAP - taken) {
            out.push_str(row);
            out.push('\n');
            taken += 1;
        }
        out.push('\n');
    }
    out
}

/// Whether `line` names `skill` as a word (not inside a longer name).
fn names_skill(line: &str, skill: &str) -> bool {
    let is_name_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    line.match_indices(skill).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + skill.len()..].chars().next();
        !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
    })
}

/// What a task's branch changed, read from git where the agent works.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// The root repository's `HEAD`.
    pub head: String,
    /// Changed files (added, modified, renamed, deleted, untracked), relative
    /// to the working directory; a submodule's under its path.
    pub files: Vec<String>,
    /// The submodules with changes of their own.
    pub submodules: Vec<String>,
    /// `git diff --stat` of each repository with changes.
    pub stat: String,
    /// The functions and sections the diff's hunks are in: `file: context`.
    pub touched: Vec<String>,
    /// Added or removed lines that are SQL.
    pub sql: Vec<String>,
    /// The diff's text (capped), which terms are matched against.
    pub diff: String,
}

/// Git's empty tree, the base of a submodule the branch added.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const DIFF_CAP: usize = 4 << 20;
const SQL_WORDS: &[&str] = &[
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "CREATE INDEX",
    "CREATE UNIQUE INDEX",
    "CREATE TRIGGER",
    "INSERT INTO",
    "DELETE FROM",
    "SELECT ",
    "UPDATE ",
];

/// What the branch in `cwd` (and each submodule) changed against where it
/// left `origin`'s default branch: commits, uncommitted changes and
/// untracked files.
pub fn read_change(cwd: &Workdir) -> Result<Change> {
    let base = worktree::new_branch_base(cwd).with_context(|| {
        format!("no origin/HEAD, origin/main or origin/master in {cwd}: cannot tell what this branch changed")
    })?;
    let merge_base = cwd
        .git(&["merge-base", &base, "HEAD"])
        .with_context(|| format!("find where this branch left {base} in {cwd}"))?;
    let mut change = Change {
        head: cwd.git(&["rev-parse", "HEAD"]).with_context(|| format!("read HEAD in {cwd}"))?,
        ..Default::default()
    };
    read_repo(cwd, "", &merge_base, &mut change)?;
    for rel in worktree::submodule_paths(cwd).context("list the submodules")? {
        // The submodule's commit where the branch began; none when the branch added it.
        let sub_base = cwd
            .git(&["rev-parse", &format!("{merge_base}:{rel}")])
            .unwrap_or_else(|_| EMPTY_TREE.to_string());
        let before = change.files.len();
        read_repo(&cwd.join(&rel), &rel, &sub_base, &mut change)?;
        if change.files.len() > before {
            change.submodules.push(rel);
        }
    }
    change.files.sort();
    change.files.dedup();
    Ok(change)
}

fn read_repo(repo: &Workdir, rel: &str, base: &str, change: &mut Change) -> Result<()> {
    let under = |f: &str| if rel.is_empty() { f.to_string() } else { format!("{rel}/{f}") };
    let run = |args: &[&str]| -> Result<Vec<u8>> {
        let out = repo.git_output(args)?;
        anyhow::ensure!(
            out.status.success(),
            "git {} failed in {repo}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(out.stdout)
    };
    let names = |args: &[&str]| -> Result<Vec<String>> {
        Ok(run(args)?
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| under(&String::from_utf8_lossy(f)))
            .collect())
    };
    let mut files = names(&["diff", "--name-only", "-z", "--ignore-submodules=all", base])?;
    files.extend(names(&["ls-files", "-o", "--exclude-standard", "-z"])?);
    if files.is_empty() {
        return Ok(());
    }
    change.files.extend(files);
    let stat = String::from_utf8_lossy(&run(&["diff", "--stat=160", "--ignore-submodules=all", base])?).to_string();
    if !stat.trim().is_empty() {
        if !rel.is_empty() {
            change.stat.push_str(&format!("In `{rel}`:\n"));
        }
        change.stat.push_str(stat.trim_end());
        change.stat.push('\n');
    }
    let diff = String::from_utf8_lossy(&run(&["diff", "-U0", "--no-color", "--ignore-submodules=all", base])?).to_string();
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = under(path);
        } else if line.starts_with("@@")
            && let Some(context) = line.splitn(3, "@@").nth(2).map(str::trim).filter(|c| !c.is_empty())
        {
            let entry = format!("{file}: {}", one_line(context, 160));
            if !change.touched.contains(&entry) {
                change.touched.push(entry);
            }
        } else if (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
            && change.sql.len() < 40
        {
            let upper = line.to_ascii_uppercase();
            if SQL_WORDS.iter().any(|w| upper.contains(w)) {
                change.sql.push(format!("{file}: {}", one_line(line, 200)));
            }
        }
    }
    let room = DIFF_CAP.saturating_sub(change.diff.len());
    change.diff.push_str(&diff.chars().take(room).collect::<String>());
    Ok(())
}

/// The areas of a change: the directories its files are in (`.` for the root).
pub fn areas(files: &[String]) -> Vec<String> {
    let set: BTreeSet<String> = files
        .iter()
        .map(|f| match f.rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => ".".to_string(),
        })
        .collect();
    set.into_iter().collect()
}

/// The app's own matches: each skill with triggers whose path glob matches a
/// changed file, else whose term the text mentions. `named` are the paths a
/// design names, matched like changed files when there is no change yet.
pub fn app_matches(skills: &[ProjectSkill], files: &[String], text: &str, design: bool) -> Vec<AppMatch> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    for skill in skills {
        let Some(triggers) = &skill.triggers else { continue };
        let by_path = triggers
            .paths
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .find_map(|p| files.iter().find(|f| crate::checks::glob_match(p, f)).map(|f| (p, f)));
        let reason = if let Some((pattern, file)) = by_path {
            if design {
                format!("the design names `{file}`, which matches `{pattern}`")
            } else {
                format!("changed file `{file}` matches `{pattern}`")
            }
        } else if let Some(term) = triggers
            .terms
            .iter()
            .map(|t| t.trim())
            .find(|t| !t.is_empty() && lower.contains(&t.to_lowercase()))
        {
            if design {
                format!("the design mentions `{term}`")
            } else {
                format!("the change mentions `{term}`")
            }
        } else {
            continue;
        };
        out.push(AppMatch { item: skill.name.clone(), kind: KIND_SKILL.to_string(), reason });
    }
    out
}

/// The path-like words in a design's text (`crates/x/y.rs`, `REVIEW.md`).
fn named_paths(text: &str) -> Vec<String> {
    let set: BTreeSet<String> = text
        .split(|c: char| c.is_whitespace() || "`'\"(),;:<>[]{}".contains(c))
        .map(|w| w.trim_end_matches('.'))
        .filter(|w| w.len() > 2 && !w.contains("://"))
        .filter(|w| w.contains('/') || w.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())))
        .map(str::to_string)
        .collect();
    set.into_iter().collect()
}

/// The checks the lifecycle config gives the node, as `<phase>/<label>`
/// with their commands.
pub fn check_candidates(conn: &rusqlite::Connection, node: Uuid) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for phase in lifecycle_config::PHASES.iter().copied().filter(|p| lifecycle_config::takes_checks(p)) {
        let Some(resolved) = lifecycle_config::resolve_checks(conn, node, phase)? else {
            continue;
        };
        for check in resolved.checks {
            let label = if check.label.trim().is_empty() { check.command.trim() } else { check.label.trim() };
            let item = format!("{phase}/{label}");
            if !out.iter().any(|(i, _)| *i == item) {
                out.push((item, check.command.trim().to_string()));
            }
        }
    }
    Ok(out)
}

/// A selection the node owes before a session of `kind` starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// [`STAGE_PREDICTED`], [`STAGE_ACTUAL`] or [`STAGE_GROWN`].
    pub stage: &'static str,
    /// What the branch changed; `None` for a predicted run (there is no change yet).
    pub change: Option<Change>,
}

/// Whether guidance is selected at all for work in `cwd`: only in a codebase
/// (as `crate::codebase_rules`), where there is a project to have guidance.
pub fn applies_in(cwd: &Workdir) -> bool {
    match cwd {
        Workdir::Host(path) => crate::codebase_rules::is_codebase(path),
        Workdir::Container { .. } | Workdir::Sandbox { .. } => true,
    }
}

/// The selection `node` owes before a session of `kind` working in `cwd`:
/// - none was made since the design was certified: a predicted one (an
///   actual one before a review);
/// - a review, and none was made against the change in this stay: an actual one;
/// - the change reached areas no actual or grown run since the latest actual
///   one was made against (after a fix, say): a grown one.
///
/// `None` for any other kind, outside a codebase, and when nothing is owed.
pub fn due(fleet: &FleetStore, node: Uuid, kind: ProtocolKind, cwd: &Workdir) -> Result<Option<Due>> {
    if !matches!(
        kind,
        ProtocolKind::Implementation | ProtocolKind::Verification | ProtocolKind::Review | ProtocolKind::Fix
    ) || !applies_in(cwd)
    {
        return Ok(None);
    }
    let (runs, certified, stay) = fleet.read(|conn| {
        let phases = PhaseRepo::new(conn);
        let certified = phases
            .list_for_node(node)?
            .into_iter()
            .filter(|e| e.state == "design" && e.kind == PHASE_CERTIFY)
            .map(|e| e.created_at)
            .max()
            .unwrap_or(0);
        Ok((store::runs(conn, node)?, certified, phases.stay_started_at(node)?.unwrap_or(0)))
    })?;
    let finished: Vec<&Run> = runs.iter().filter(|r| r.is_finished() && r.started_at >= certified).collect();
    if finished.is_empty() {
        return Ok(Some(if kind == ProtocolKind::Review {
            Due { stage: STAGE_ACTUAL, change: Some(read_change(cwd)?) }
        } else {
            Due { stage: STAGE_PREDICTED, change: None }
        }));
    }
    if kind == ProtocolKind::Review && !finished.iter().any(|r| r.stage == STAGE_ACTUAL && r.started_at >= stay) {
        return Ok(Some(Due { stage: STAGE_ACTUAL, change: Some(read_change(cwd)?) }));
    }
    let Some(actual) = finished.iter().rev().find(|r| r.stage == STAGE_ACTUAL) else {
        return Ok(None);
    };
    let covered: BTreeSet<&String> = finished
        .iter()
        .filter(|r| r.started_at >= actual.started_at && (r.stage == STAGE_ACTUAL || r.stage == STAGE_GROWN))
        .flat_map(|r| r.areas.iter())
        .collect();
    let change = read_change(cwd)?;
    if areas(&change.files).iter().all(|a| covered.contains(a)) {
        return Ok(None);
    }
    Ok(Some(Due { stage: STAGE_GROWN, change: Some(change) }))
}

/// The files every reviewer is asked to read, when they exist: the root's
/// `REVIEW.md`, and each submodule's `CLAUDE.md` and `REVIEW.md` (every
/// submodule before there is a change; the touched ones after).
fn always_read_wanted(cwd: &Workdir, change: Option<&Change>) -> Result<Vec<String>> {
    let submodules = match change {
        Some(change) => change.submodules.clone(),
        None => worktree::submodule_paths(cwd).context("list the submodules")?,
    };
    let mut wanted = vec!["REVIEW.md".to_string()];
    for sub in submodules {
        wanted.push(format!("{sub}/CLAUDE.md"));
        wanted.push(format!("{sub}/REVIEW.md"));
    }
    Ok(wanted)
}

/// The run the node owes as `due` says, ready to start: the candidates, the
/// app's matches and the selector's material. Reads the project where the
/// agent works, so never on the UI thread.
pub fn prepare_run(fleet: &FleetStore, node: Uuid, due: &Due, cwd: &Workdir) -> Result<NewRun> {
    let wanted = always_read_wanted(cwd, due.change.as_ref())?;
    let survey = survey(cwd, &wanted)?;
    let (design, checks, current) = fleet.read(|conn| {
        let nodes = NodeRepo::new(conn);
        let mut design = String::new();
        for key in ["design", EXTRA_CONTENT_DETAILS] {
            if let Some(text) = nodes.get_extra_content(node, key)?.filter(|t| !t.trim().is_empty()) {
                design.push_str(text.trim());
                design.push_str("\n\n");
            }
        }
        for obligation in ObligationRepo::new(conn).list_for_node(node)? {
            design.push_str(&obligation.body);
            design.push('\n');
        }
        Ok((design, check_candidates(conn, node)?, store::live(conn, node)?))
    })?;
    let (matches, change_ref, run_areas) = match &due.change {
        Some(change) => (
            app_matches(&survey.skills, &change.files, &change.diff, false),
            change.head.clone(),
            areas(&change.files),
        ),
        None => (
            app_matches(&survey.skills, &named_paths(&design), &design, true),
            store::CHANGE_DESIGN.to_string(),
            Vec::new(),
        ),
    };
    let candidates = Candidates {
        skills: survey.skills.iter().filter(|s| s.triggers.is_none()).map(|s| s.name.clone()).collect(),
        files: survey.files.clone(),
        checks: checks.iter().map(|(item, _)| item.clone()).collect(),
    };
    let material = material(due, &survey, &design, &checks, &matches, &current);
    Ok(NewRun {
        node_id: node,
        stage: due.stage.to_string(),
        change_ref,
        areas: run_areas,
        candidates,
        material,
        app_matches: matches,
    })
}

fn capped(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (cut at {cap} bytes)", &text[..end])
}

/// What the selector's opening shows: the change (or the design), the
/// candidates, the routing table, the lenses, and what is already selected.
fn material(
    due: &Due,
    survey: &Survey,
    design: &str,
    checks: &[(String, String)],
    matches: &[AppMatch],
    current: &[Guidance],
) -> String {
    let mut out = format!("Stage: **{}**.\n\n", due.stage);
    match &due.change {
        Some(change) => {
            out.push_str("### The change\n\n");
            let shown = 200;
            out.push_str(&format!("{} files changed:\n\n", change.files.len()));
            for file in change.files.iter().take(shown) {
                out.push_str(&format!("- `{file}`\n"));
            }
            if change.files.len() > shown {
                out.push_str(&format!("- … and {} more\n", change.files.len() - shown));
            }
            if !change.stat.trim().is_empty() {
                out.push_str(&format!("\n```\n{}\n```\n", capped(change.stat.trim_end(), 12_000)));
            }
            if !change.touched.is_empty() {
                out.push_str("\nFunctions and sections touched:\n\n");
                for entry in change.touched.iter().take(150) {
                    out.push_str(&format!("- {entry}\n"));
                }
            }
            if !change.sql.is_empty() {
                out.push_str("\nSQL added or removed:\n\n");
                for line in &change.sql {
                    out.push_str(&format!("- `{}`\n", line.replace('`', "'")));
                }
            }
        }
        None => {
            out.push_str(
                "### The design\n\nThere is no change yet: match against the areas the design and the \
                 obligations name.\n\n",
            );
            out.push_str(&capped(design.trim(), 24_000));
            out.push('\n');
        }
    }
    out.push_str("\n### Skill candidates\n\n");
    let offered: Vec<&ProjectSkill> = survey.skills.iter().filter(|s| s.triggers.is_none()).collect();
    if offered.is_empty() {
        out.push_str("(none)\n");
    }
    for skill in offered {
        let description = if skill.description.is_empty() { "(no description)" } else { &skill.description };
        out.push_str(&format!("- `{}`: {description}\n", skill.name));
    }
    let decided: Vec<&ProjectSkill> = survey.skills.iter().filter(|s| s.triggers.is_some()).collect();
    if !decided.is_empty() {
        out.push_str("\nSkills the app decides by their own triggers (not yours to select):\n\n");
        for skill in decided {
            match matches.iter().find(|m| m.item == skill.name) {
                Some(m) => out.push_str(&format!("- `{}`: selected, {}\n", skill.name, m.reason)),
                None => out.push_str(&format!("- `{}`: not triggered\n", skill.name)),
            }
        }
    }
    out.push_str("\n### File candidates\n\nRead by every session when selected.\n\n");
    if survey.files.is_empty() {
        out.push_str("(none)\n");
    }
    for file in &survey.files {
        out.push_str(&format!("- `{file}`\n"));
    }
    out.push_str("\n### Check candidates\n\n");
    if checks.is_empty() {
        out.push_str("(none)\n");
    }
    for (item, command) in checks {
        out.push_str(&format!("- `{item}`: `{}`\n", command.replace('`', "'")));
    }
    let routing = routing_table(survey);
    if !routing.trim().is_empty() {
        out.push_str("\n### The project's routing table\n\n");
        out.push_str(&routing);
    }
    out.push_str("\n### Lenses\n\nRecord every one, as applying or not.\n\n");
    for lens in tod_store::review::LENSES.iter() {
        out.push_str(&format!("- `{}`: {}\n", lens.slug, lens.title));
    }
    let kept: Vec<&Guidance> = current.iter().collect();
    if !kept.is_empty() {
        out.push_str("\n### Selected so far\n\n");
        if due.stage == STAGE_GROWN {
            out.push_str("These stay; add only what the new areas need.\n\n");
        } else {
            out.push_str(
                "Select again what still applies; what you do not select again is dropped, except \
                 what the user chose.\n\n",
            );
        }
        for g in kept {
            out.push_str(&selection_line(g));
        }
    }
    out
}

fn selection_line(g: &Guidance) -> String {
    let what = match (g.kind.as_str(), g.applies) {
        (KIND_LENS, true) => format!("lens `{}` applies", g.item),
        (KIND_LENS, false) => format!("lens `{}` does not apply", g.item),
        (kind, _) => format!("{kind} `{}`", g.item),
    };
    format!("- {what}: {} (by {}, {})\n", g.reason.trim(), g.selected_by_label(), g.stage)
}

/// The selector's opening for the node's open run.
pub fn opening_message(fleet: &FleetStore, media: &MediaPaths, data_root: &Path, node: Uuid) -> Result<String> {
    let (row, summary, lifecycle, obligations, run) = fleet.read(|conn| {
        let nodes = NodeRepo::new(conn);
        let row = nodes.get(node)?.with_context(|| format!("node {node} not found"))?;
        let summary = match nodes.get_summary(node)? {
            Some(summary) if !summary.body.trim().is_empty() => Some(summary.body),
            _ => nodes.get_extra_content(node, EXTRA_CONTENT_DETAILS)?,
        };
        Ok((
            row,
            summary,
            nodes.get_lifecycle(node)?,
            ObligationRepo::new(conn).list_for_node(node)?,
            store::open_run(conn, node)?,
        ))
    })?;
    let run = run.with_context(|| format!("no guidance selection is running for node {node}"))?;
    let selection = NodeSelection {
        id: row.id,
        title: row.title,
        body: summary,
        lifecycle,
        slug: Some(row.slug),
    };
    let pull_requests = crate::pr_context::for_node(fleet, node);
    let ctx = DynamicContext {
        data_root: Some(data_root),
        node: Some(&selection),
        obligations: &obligations,
        guidance: &run.material,
        pull_requests: &pull_requests,
        ..Default::default()
    };
    build_message(media, &GUIDANCE, None, &ctx, "")
}

/// What a selected file's text could not be read says, so a caller with only
/// the error's text can tell the project's setup from the agent failing.
pub const MISSING_FILE_MARKER: &str = "was selected as guidance but cannot be read";

/// The "Project guidance" block for the node's current selection, each
/// selected skill's and file's text in it, the same for every session that
/// gets it (implement, verify, every reviewer, fix). Empty when nothing is
/// selected. Reads where the agent works, so never on the UI thread.
pub fn render_selection(fleet: &FleetStore, node: Uuid, cwd: &Workdir) -> Result<String> {
    let live = fleet.read(|conn| store::live(conn, node))?;
    if live.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::from(
        "\n## Project guidance\n\nThe project's own instructions that this task's change needs were \
         selected for it, and their text is below. They are part of the job, not an appendix: do the \
         work, and judge it, the way they direct, as you would anything else in this message. Where one \
         and this message disagree on how the project's code is written, the project's instruction wins; \
         what you must record, and how, is this message's.\n\n",
    );
    let reading: Vec<&Guidance> = live.iter().filter(|g| g.is_reading()).collect();
    if !reading.is_empty() {
        out.push_str("Selected to read:\n\n");
        for g in &reading {
            out.push_str(&selection_line(g));
        }
        out.push('\n');
    }
    let lenses: Vec<&Guidance> = live.iter().filter(|g| g.kind == KIND_LENS).collect();
    if !lenses.is_empty() {
        out.push_str("Lenses:\n\n");
        for g in &lenses {
            out.push_str(&selection_line(g));
        }
        out.push('\n');
    }
    let checks: Vec<&Guidance> = live.iter().filter(|g| g.kind == KIND_CHECK).collect();
    if !checks.is_empty() {
        out.push_str("Checks that apply:\n\n");
        for g in &checks {
            out.push_str(&selection_line(g));
        }
        out.push('\n');
    }
    let own = crate::skills::SkillsPaths::discover()?;
    for g in reading {
        match g.kind.as_str() {
            KIND_SKILL => {
                let skill = crate::skills_context::resolve(&own, cwd, &g.item)
                    .with_context(|| format!("the skill `{}` selected as this task's guidance", g.item))?;
                out.push_str(&crate::skills_context::render_skill(&skill));
            }
            _ => {
                out.push_str(&format!("\n{}\n\n", store::inline_heading(KIND_FILE, &g.item)));
                out.push_str(read_file(cwd, &g.item)?.trim());
                out.push('\n');
            }
        }
    }
    Ok(out)
}

fn read_file(cwd: &Workdir, rel: &str) -> Result<String> {
    let missing = |why: String| {
        anyhow::anyhow!(
            "`{rel}` {MISSING_FILE_MARKER} in {}: {why}. Restore it, or remove it from the task's guidance",
            cwd.storage()
        )
    };
    if rel.split('/').any(|part| part == "..") || rel.starts_with('/') {
        return Err(missing("it is not a path inside the working directory".to_string()));
    }
    match cwd {
        Workdir::Host(dir) => std::fs::read_to_string(dir.join(rel)).map_err(|e| missing(e.to_string())),
        _ => {
            let out = cwd
                .output("sh", &["-c", "cat -- \"$1\"", "sh", rel])
                .with_context(|| format!("could not read `{rel}` in {}", cwd.storage()))?;
            if !out.status.success() {
                return Err(missing(String::from_utf8_lossy(&out.stderr).trim().to_string()));
            }
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
    }
}

/// The names of the skills the node's selection inlines, so the Skills block
/// does not repeat their text.
pub fn selected_skills(fleet: &FleetStore, node: Uuid) -> Result<Vec<String>> {
    Ok(fleet
        .read(|conn| store::live(conn, node))?
        .into_iter()
        .filter(|g| g.kind == KIND_SKILL && g.applies)
        .map(|g| g.item)
        .collect())
}

#[cfg(test)]
mod tests;
