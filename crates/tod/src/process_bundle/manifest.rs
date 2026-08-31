//! Bundled interview agent doc paths (formerly manifest.yaml).

use super::install::TodInstallPaths;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct PhaseAgents {
    pub base: String,
    pub question_maker: String,
    pub answer_processor: String,
}

#[derive(Debug, Clone)]
pub struct ProcessManifest {
    process_root: PathBuf,
    agents: PhaseAgents,
}

impl ProcessManifest {
    const BASE: &'static str = "agents/interview/base.md";
    const QUESTION_MAKER: &'static str = "agents/interview/question-maker.md";
    const ANSWER_PROCESSOR: &'static str = "agents/interview/answer-processor.md";
    const DEEP_DIVE: &'static str = "agents/interview/deep-dive.md";
    const PHASE_PROPOSED: &'static str = "agents/interview/phases/proposed.md";
    const PHASE_DESIGN: &'static str = "agents/interview/phases/design.md";
    const PHASE_PLANNING: &'static str = "agents/interview/phases/planning.md";

    const KNOWN_PHASES: &'static [&'static str] = &[
        "task-requirements-interview",
        "design-interview",
        "planning-interview",
        "project-defining",
    ];

    pub fn load(install: &TodInstallPaths) -> Result<Self> {
        let root = install.process_root();
        for rel in [
            Self::BASE,
            Self::QUESTION_MAKER,
            Self::ANSWER_PROCESSOR,
            Self::DEEP_DIVE,
            Self::PHASE_PROPOSED,
            Self::PHASE_DESIGN,
            Self::PHASE_PLANNING,
        ] {
            let path = root.join(rel);
            if !path.is_file() {
                anyhow::bail!("missing bundled process doc {}", path.display());
            }
        }
        Ok(Self {
            process_root: root.to_path_buf(),
            agents: PhaseAgents {
                base: Self::BASE.to_string(),
                question_maker: Self::QUESTION_MAKER.to_string(),
                answer_processor: Self::ANSWER_PROCESSOR.to_string(),
            },
        })
    }

    pub fn phase(&self, base_phase: &str) -> Result<&PhaseAgents> {
        if Self::KNOWN_PHASES.contains(&base_phase) {
            Ok(&self.agents)
        } else {
            anyhow::bail!("unknown interview phase: {base_phase}")
        }
    }

    pub fn question_maker_doc(&self, base_phase: &str) -> Result<PathBuf> {
        self.phase(base_phase)?;
        Ok(self.resolve(Self::QUESTION_MAKER))
    }

    pub fn answer_processor_doc(&self, base_phase: &str) -> Result<PathBuf> {
        self.phase(base_phase)?;
        Ok(self.resolve(Self::ANSWER_PROCESSOR))
    }

    /// Deep-dive chat role doc (not phase-specific).
    pub fn deep_dive_doc(&self) -> PathBuf {
        self.resolve(Self::DEEP_DIVE)
    }

    /// Shared interview conventions (question maker + answer processor).
    pub fn base_doc(&self, base_phase: &str) -> Result<PathBuf> {
        self.phase(base_phase)?;
        Ok(self.resolve(Self::BASE))
    }

    /// Interview phase context (requirements / design / planning).
    pub fn interview_phase_doc(&self, base_phase: &str) -> Result<PathBuf> {
        self.phase(base_phase)?;
        let rel = match base_phase {
            "task-requirements-interview" | "project-defining" => Self::PHASE_PROPOSED,
            "design-interview" => Self::PHASE_DESIGN,
            "planning-interview" => Self::PHASE_PLANNING,
            other => anyhow::bail!("no interview phase doc for {other}"),
        };
        Ok(self.resolve(rel))
    }

    pub fn phase_count(&self) -> usize {
        Self::KNOWN_PHASES.len()
    }

    pub fn known_phases(&self) -> impl Iterator<Item = &'static str> {
        Self::KNOWN_PHASES.iter().copied()
    }

    /// Lifecycle state agent doc when present (`agents/state/{lifecycle}.md`).
    pub fn state_doc(&self, lifecycle: &str) -> Option<PathBuf> {
        let path = self.resolve(&format!("agents/state/{lifecycle}.md"));
        if path.is_file() { Some(path) } else { None }
    }

    /// Shared state-agent conventions.
    pub fn state_base_doc(&self) -> PathBuf {
        self.resolve("agents/state/base.md")
    }

    fn resolve(&self, rel: &str) -> PathBuf {
        self.process_root.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_bundled_defaults() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("process");
        if !root.join("README.md").is_file() {
            return;
        }
        let install = TodInstallPaths::from_process_root(root).unwrap();
        let manifest = ProcessManifest::load(&install).unwrap();
        assert_eq!(manifest.phase_count(), 4);
        assert!(
            manifest
                .question_maker_doc("design-interview")
                .unwrap()
                .ends_with("question-maker.md")
        );
        assert!(
            manifest
                .interview_phase_doc("design-interview")
                .unwrap()
                .ends_with("phases/design.md")
        );
        assert!(
            manifest
                .interview_phase_doc("task-requirements-interview")
                .unwrap()
                .ends_with("phases/proposed.md")
        );
    }
}
