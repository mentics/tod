//! `batch run` must work in an agent shell that gives a command no stdin
//! (gap 106: a Codex agent's heredoc arrived empty), and say so when it
//! got nothing.

use std::process::{Command, Stdio};

fn run(root: &std::path::Path, args: &[&str], stdin: Stdio) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tod-cli"))
        .arg("--data-root")
        .arg(root)
        .args(["batch", "run"])
        .args(args)
        .env_remove("TOD_DATA_ROOT")
        .stdin(stdin)
        .output()
        .expect("run tod-cli")
}

#[test]
fn an_empty_stdin_names_the_forms_that_need_none() {
    let root = tempfile::tempdir().unwrap();
    let out = run(root.path(), &[], Stdio::null());
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("script on stdin was empty"), "{err}");
    assert!(err.contains("--file"), "{err}");
    assert!(err.contains("--script"), "{err}");
    assert!(!err.contains("the script has no commands"), "{err}");
}

#[test]
fn the_script_can_come_from_a_file_or_an_argument_with_no_stdin() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("script.txt");
    std::fs::write(&file, "help node\n").unwrap();
    let out = run(root.path(), &["--file", file.to_str().unwrap()], Stdio::null());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("batch:"), "{out:?}");

    let out = run(root.path(), &["--script", "help node"], Stdio::null());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_comment_only_script_is_named_as_such() {
    let root = tempfile::tempdir().unwrap();
    let out = run(root.path(), &["--script", "# nothing\nnode list"], Stdio::null());
    // A script with a command is not empty (it may still fail for the data root).
    assert!(!String::from_utf8_lossy(&out.stderr).contains("empty"));
    let out = run(root.path(), &["--script", "# only a note"], Stdio::null());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no commands"));
}
