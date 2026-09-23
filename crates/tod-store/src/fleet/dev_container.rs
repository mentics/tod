//! Where a node's launches run: this machine, or the dev container its Files
//! capability names.
//!
//! [`launch_for`] only describes the launch; `tod_agent::devcontainer::prepare`
//! checks it against the running container (off the UI thread).

use crate::fleet::cli_relay;
use crate::fleet::node_actions::ResolvedFiles;
use crate::fleet::workdir::Workdir;
use anyhow::Result;
use std::path::Path;
use tod_agent::AgentEnvironment;
use tod_agent::devcontainer::{ContainerFile, DevContainerLaunch};

/// The container path of the `tod-cli` shim.
pub fn shim_path() -> String {
    format!("{}/tod-cli", cli_relay::SHIM_DIR)
}

/// The dev container launch for a process from a node with `files` that
/// runs in `cwd`. A directory inside a container always launches there. A
/// directory on this machine launches in the node's dev container only when
/// its repository is mounted into one and `cwd` is in its Files directory
/// (not the data root, for a turn that needs no workspace); `None` means
/// this machine. Starts the `tod-cli` relay.
pub fn launch_for(
    files: Option<&ResolvedFiles>,
    cwd: &Workdir,
    data_root: &Path,
) -> Result<Option<DevContainerLaunch>> {
    let (container, host_dir, directory) = match cwd {
        Workdir::Container { container, path } => {
            (container.clone(), data_root.to_path_buf(), Some(path.clone()))
        }
        Workdir::Host(host_cwd) => {
            let Some(files) = files else {
                return Ok(None);
            };
            let Some(dev) = files.dev_container.as_ref().filter(|dev| dev.repo_on_host) else {
                return Ok(None);
            };
            let Some(container) = dev.container() else {
                return Ok(None);
            };
            let Some(Workdir::Host(root)) = files.ready_directory() else {
                return Ok(None);
            };
            if !host_cwd.starts_with(&root) {
                return Ok(None);
            }
            // The directory inside follows from the container's mounts.
            (container.to_string(), host_cwd.clone(), None)
        }
    };
    let relay = cli_relay::ensure_started(data_root)?;
    Ok(Some(DevContainerLaunch {
        container,
        host_dir,
        directory,
        env: relay.env(),
        path_prepend: vec![cli_relay::SHIM_DIR.to_string()],
        files: vec![ContainerFile {
            path: shim_path(),
            contents: cli_relay::SHIM_SCRIPT.to_string(),
            executable: true,
        }],
    }))
}

/// [`launch_for`] as the environment an agent runs in.
pub fn environment_for(
    files: Option<&ResolvedFiles>,
    cwd: &Workdir,
    data_root: &Path,
) -> Result<AgentEnvironment> {
    Ok(match launch_for(files, cwd, data_root)? {
        Some(launch) => AgentEnvironment::DevContainer(launch),
        None => AgentEnvironment::Host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::node_files::DevContainerSetting;
    use std::path::PathBuf;

    fn files(dir: &Path, dev: Option<DevContainerSetting>) -> ResolvedFiles {
        ResolvedFiles {
            source_node_id: "n".into(),
            source_title: "n".into(),
            inherited: false,
            repo: Some(dir.to_string_lossy().into_owned()),
            branch: None,
            use_worktree: false,
            worktree_path: None,
            worktree_lease_id: None,
            worktree_lease_holder: None,
            dev_container: dev,
        }
    }

    #[test]
    fn a_mounted_repository_launches_in_the_container_only_from_its_directory() {
        let dir = std::env::temp_dir();
        let cwd = Workdir::host(&dir);
        let data_root = PathBuf::from("/data");
        let dev = DevContainerSetting {
            container: Some("my-dev".into()),
            repo_on_host: true,
        };
        let on_host = files(&dir, None);
        assert!(launch_for(Some(&on_host), &cwd, &data_root).unwrap().is_none());

        let mounted = files(&dir, Some(dev));
        assert!(
            launch_for(Some(&mounted), &Workdir::host("/elsewhere"), &data_root)
                .unwrap()
                .is_none()
        );
        let launch = launch_for(Some(&mounted), &cwd, &data_root)
            .unwrap()
            .expect("container launch");
        assert_eq!(launch.container, "my-dev");
        assert_eq!(launch.directory, None);
        assert!(launch.env.iter().any(|(k, _)| k == cli_relay::PORT_ENV));
        assert_eq!(launch.files[0].path, shim_path());

        let unchosen = files(&dir, Some(DevContainerSetting::default()));
        assert!(launch_for(Some(&unchosen), &cwd, &data_root).unwrap().is_none());
    }

    #[test]
    fn a_directory_in_a_container_launches_there() {
        let data_root = PathBuf::from("/data");
        let cwd = Workdir::container("my-dev", "/workspaces/app/.worktrees/x");
        let launch = launch_for(None, &cwd, &data_root)
            .unwrap()
            .expect("container launch");
        assert_eq!(launch.container, "my-dev");
        assert_eq!(
            launch.directory.as_deref(),
            Some("/workspaces/app/.worktrees/x")
        );
        assert_eq!(launch.host_dir, data_root);
    }
}
