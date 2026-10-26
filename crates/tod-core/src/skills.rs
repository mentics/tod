//! Skills for working with tod itself, shipped alongside the executable.
//!
//! Unlike the agent context fragments (`crate::media`), nothing here is put in
//! an agent's context: a skill is read only when an agent runs into something
//! it does not know how to do (`tod-cli skills`). They are versioned with the
//! source under `assets/skills/` and installed as a `skills/` sibling of the
//! executable. `README.md` lists each skill and when to use it; every other
//! `<name>.md` is one skill.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Root of the installed `skills/` bundle.
#[derive(Debug, Clone)]
pub struct SkillsPaths {
    root: PathBuf,
}

impl SkillsPaths {
    /// Resolution order:
    /// 1. `TOD_SKILLS_ROOT` env override
    /// 2. `{executable_dir}/skills` (installed layout)
    /// 3. `{executable_dir}/../skills` (a cloud sandbox: `tod-cli` is in
    ///    `/opt/tod/bin`, the bundles in `/opt/tod`)
    /// 4. Walk up from cwd for `assets/skills` (dev checkout)
    pub fn discover() -> Result<Self> {
        if let Ok(raw) = std::env::var("TOD_SKILLS_ROOT") {
            return Self::from_root(PathBuf::from(raw));
        }
        if let Some(dir) = tod_store::install::program_dir() {
            let candidate = dir.join("skills");
            if candidate.join("README.md").is_file() {
                return Self::from_root(candidate);
            }
            if let Some(parent) = dir.parent() {
                let candidate = parent.join("skills");
                if candidate.join("README.md").is_file() {
                    return Self::from_root(candidate);
                }
            }
        }
        let start = std::env::current_dir().context("failed to read current directory")?;
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            let candidate = current.join("assets").join("skills");
            if candidate.join("README.md").is_file() {
                return Self::from_root(candidate);
            }
            dir = current.parent();
        }
        anyhow::bail!(
            "could not locate the skills bundle (set TOD_SKILLS_ROOT, or install \
             skills/ next to the executable)"
        )
    }

    pub fn from_root(root: PathBuf) -> Result<Self> {
        if !root.join("README.md").is_file() {
            anyhow::bail!("{} has no README.md", root.display());
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The README listing every skill and when to use it.
    pub fn readme(&self) -> Result<String> {
        let path = self.root.join("README.md");
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// The names of the skills (every `<name>.md` but the README), sorted.
    pub fn names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .with_context(|| format!("reading {}", self.root.display()))?
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let stem = name.strip_suffix(".md")?;
                (stem != "README").then(|| stem.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// One skill's text. An unknown name is an error naming the ones there are.
    pub fn skill(&self, name: &str) -> Result<String> {
        if name.is_empty() || name.contains(['/', '\\', '.']) {
            anyhow::bail!("`{name}` is not a skill name");
        }
        let path = self.root.join(format!("{name}.md"));
        std::fs::read_to_string(&path).map_err(|_| {
            anyhow::anyhow!(
                "no skill named `{name}` (skills: {})",
                self.names().unwrap_or_default().join(", ")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_reads_and_refuses_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "index").unwrap();
        std::fs::write(dir.path().join("journeys.md"), "how").unwrap();
        let paths = SkillsPaths::from_root(dir.path().to_path_buf()).unwrap();
        assert_eq!(paths.readme().unwrap(), "index");
        assert_eq!(paths.names().unwrap(), vec!["journeys"]);
        assert_eq!(paths.skill("journeys").unwrap(), "how");
        assert!(paths.skill("../README").is_err());
        assert!(paths.skill("nope").unwrap_err().to_string().contains("journeys"));
    }

    /// Every skill in the source tree is listed in the README, and the README
    /// lists none that does not exist.
    #[test]
    fn the_shipped_readme_lists_exactly_the_shipped_skills() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills");
        let paths = SkillsPaths::from_root(root).unwrap();
        let readme = paths.readme().unwrap();
        for name in paths.names().unwrap() {
            assert!(readme.contains(&format!("`{name}`")), "README does not list `{name}`");
        }
    }
}
