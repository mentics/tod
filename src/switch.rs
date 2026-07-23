//! Switch-to-task workflow: activate worktree branches and open Cursor.

use std::path::Path;

use color_eyre::eyre::{Context, eyre};

use crate::gitutil;
use crate::task::Worktree;
use crate::treehouse;

/// Check out the task branch (or `temp{N}`) in the worktree main repo and each submodule.
///
/// `on_progress` is called with a human-readable step label before each checkout.
pub fn activate_worktree(
    worktree: &Worktree,
    task_modules: &[String],
    branch: &str,
    mut on_progress: impl FnMut(String) -> color_eyre::Result<()>,
) -> color_eyre::Result<()> {
    if branch.is_empty() {
        return Err(eyre!("cannot activate worktree without a branch name"));
    }

    let root = &worktree.path;
    if !root.is_dir() {
        return Err(eyre!("worktree path does not exist: {}", root.display()));
    }

    let main_name = gitutil::main_repo_name(root)?;
    let temp_branch = format!("temp{}", worktree.number);

    let main_target = if task_modules.iter().any(|m| m == &main_name) {
        branch
    } else {
        temp_branch.as_str()
    };
    on_progress(format!(
        "Activate worktree: main `{main_name}` → `{main_target}`"
    ))?;
    gitutil::checkout_or_create_branch(root, main_target)
        .wrap_err_with(|| format!("activating main repo `{}` onto `{main_target}`", main_name))?;

    for (name, rel) in gitutil::submodule_entries(root)? {
        let sub_path = root.join(&rel);
        if !sub_path.is_dir() {
            return Err(eyre!(
                "submodule `{name}` path missing in worktree: {}",
                sub_path.display()
            ));
        }
        let target = if task_modules.iter().any(|m| m == &name) {
            branch
        } else {
            temp_branch.as_str()
        };
        on_progress(format!(
            "Activate worktree: submodule `{name}` → `{target}`"
        ))?;
        gitutil::checkout_or_create_branch(&sub_path, target)
            .wrap_err_with(|| format!("activating submodule `{name}` onto `{target}`"))?;
    }

    Ok(())
}

/// Lease a new Treehouse worktree from `cwd` (main repo).
pub fn lease_new_worktree(cwd: impl AsRef<Path>) -> color_eyre::Result<Worktree> {
    let leased = treehouse::lease_worktree(cwd.as_ref())?;
    Ok(leased.into())
}

/// Open Cursor on the worktree path (detached; does not wait).
pub fn launch_cursor(worktree: &Worktree) -> color_eyre::Result<()> {
    treehouse::launch_cursor(&worktree.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitutil;
    use crate::task::Worktree;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "test@example.com"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "Test"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        fs::write(dir.join("README"), "hi").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "README"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-m", "init"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn activate_checks_out_task_or_temp_branch() {
        let dir = std::env::temp_dir().join(format!("tod-switch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        init_repo(&dir);
        let main = gitutil::main_repo_name(&dir).unwrap();

        let wt = Worktree {
            number: 7,
            path: PathBuf::from(&dir),
        };

        // Main module selected → task branch.
        activate_worktree(&wt, &[main.clone()], "feat/switch-test", |_| Ok(())).unwrap();
        let head = gitutil::git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "feat/switch-test");

        // Main not selected → temp7.
        activate_worktree(&wt, &[], "feat/switch-test", |_| Ok(())).unwrap();
        let head = gitutil::git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head.trim(), "temp7");

        let _ = fs::remove_dir_all(&dir);
    }
}
