//! Settings snapshot embedded in every bundle (doc/journeys/spec.md §7).
//!
//! Settings drive a large share of errors (wrong path, wrong model), so the
//! snapshot carries the whole `TodSettings` plus everything resolved from it:
//! per-role agent platform/model/effort, the roots the app resolved at
//! startup, the `tod-cli` path and build stamp, and a node's dev container
//! settings when the snapshot is for a node.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use tod_agent::platform::AgentPlatform;
use tod_store::fleet::repos::node_files::DevContainerSetting;
use tod_store::settings::{AgentRole, TodSettings};

use crate::media::MediaPaths;
use crate::process_bundle::TodInstallPaths;

/// Resolved agent platform, model, and effort for one [`AgentRole`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAgentSettings {
    pub role: String,
    pub platform: AgentPlatform,
    pub model: String,
    pub effort: String,
}

/// A full settings snapshot: the raw settings plus everything resolved from
/// them, taken at bundle-build time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    /// The whole settings file, as configured.
    pub settings: TodSettings,
    /// Resolved agent platform/model/effort, one entry per [`AgentRole`].
    pub agents: Vec<ResolvedAgentSettings>,
    /// Durable state root (`tod.db`, `tod.yml`, …).
    pub data_root: PathBuf,
    /// Bundled `process/` root (agent behavior docs).
    pub process_root: PathBuf,
    /// Bundled `media/` root (agent context docs).
    pub media_root: PathBuf,
    /// Path to the `tod-cli` binary the agent shells out to.
    pub cli_path: PathBuf,
    /// `tod-cli`'s build stamp, so a mismatch with the running app's own
    /// stamp is visible in the snapshot (see `tod_core::CLI_BUILD_STAMP`).
    pub cli_build_stamp: String,
    /// The node's dev container settings, when the snapshot is for a node
    /// that has Files capability settings.
    pub dev_container: Option<DevContainerSetting>,
}

/// Builds a settings snapshot for a bundle.
///
/// `node` is the node's dev container setting (`NodeFiles::dev_container`,
/// see `tod_store::fleet::repos::node_files`), when the snapshot is for a
/// node rather than the project as a whole.
pub fn settings_snapshot(
    install: &TodInstallPaths,
    media: &MediaPaths,
    data_root: &std::path::Path,
    cli_path: &std::path::Path,
    settings: &TodSettings,
    node: Option<&DevContainerSetting>,
) -> SettingsSnapshot {
    let agents = AgentRole::ALL
        .iter()
        .map(|&role| {
            let options = settings.launch_options_for(role);
            ResolvedAgentSettings {
                role: format!("{role:?}"),
                platform: options.platform,
                model: options.model,
                effort: options.effort,
            }
        })
        .collect();

    SettingsSnapshot {
        settings: settings.clone(),
        agents,
        data_root: data_root.to_path_buf(),
        process_root: install.process_root().to_path_buf(),
        media_root: media.media_root().to_path_buf(),
        cli_path: cli_path.to_path_buf(),
        cli_build_stamp: crate::CLI_BUILD_STAMP.to_string(),
        dev_container: node.cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal self-cleaning temp directory (mirrors `media::tests::tempdir`).
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("tod-snapshot-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn install_paths(dir: &std::path::Path) -> TodInstallPaths {
        std::fs::write(dir.join("README.md"), "process").unwrap();
        TodInstallPaths::from_process_root(dir.to_path_buf()).expect("install paths")
    }

    fn media_paths(dir: &std::path::Path) -> MediaPaths {
        std::fs::create_dir_all(dir.join("context")).unwrap();
        MediaPaths::from_media_root(dir.to_path_buf()).expect("media paths")
    }

    #[test]
    fn snapshot_resolves_agent_settings_for_every_role() {
        let settings = TodSettings::default();
        let process_dir = Dir::new();
        let media_dir = Dir::new();
        let install = install_paths(process_dir.path());
        let media = media_paths(media_dir.path());

        let snapshot = settings_snapshot(
            &install,
            &media,
            std::path::Path::new("/tmp/data"),
            std::path::Path::new("/tmp/data/tod-cli"),
            &settings,
            None,
        );

        assert_eq!(snapshot.agents.len(), AgentRole::ALL.len());
        for role in AgentRole::ALL {
            let expected = settings.launch_options_for(role);
            let resolved = snapshot
                .agents
                .iter()
                .find(|a| a.role == format!("{role:?}"))
                .unwrap_or_else(|| panic!("no resolved settings for {role:?}"));
            assert_eq!(resolved.platform, expected.platform);
            assert_eq!(resolved.model, expected.model);
            assert_eq!(resolved.effort, expected.effort);
        }

        assert_eq!(snapshot.cli_build_stamp, crate::CLI_BUILD_STAMP);
        assert!(snapshot.dev_container.is_none());
    }

    #[test]
    fn snapshot_carries_the_nodes_dev_container_setting() {
        let settings = TodSettings::default();
        let process_dir = Dir::new();
        let media_dir = Dir::new();
        let install = install_paths(process_dir.path());
        let media = media_paths(media_dir.path());
        let dev_container = DevContainerSetting {
            container: Some("my-dev".into()),
            repo_on_host: false,
        };

        let snapshot = settings_snapshot(
            &install,
            &media,
            std::path::Path::new("/tmp/data"),
            std::path::Path::new("/tmp/data/tod-cli"),
            &settings,
            Some(&dev_container),
        );

        assert_eq!(snapshot.dev_container, Some(dev_container));
    }
}
