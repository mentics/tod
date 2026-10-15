//! A failing `tod-cli` run is recorded in the data root's problem log, where
//! the app's tail picks it up (log panel and toast).

use std::process::Command;

fn run(root: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tod-cli"))
        .arg("--data-root")
        .arg(root)
        .args(args)
        .env_remove("TOD_DATA_ROOT")
        .output()
        .expect("run tod-cli")
}

fn lines(root: &std::path::Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(tod_problems::dir(root).join("tod-cli.jsonl")).unwrap_or_default();
    text.lines().map(|l| serde_json::from_str(l).expect("json line")).collect()
}

#[test]
fn an_error_exit_is_recorded_as_a_problem() {
    let root = std::env::temp_dir().join(format!("tod-cli-problems-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let out = run(&root, &["no-such-noun"]);
    assert!(!out.status.success());
    // stderr is unchanged for humans and agents.
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown noun"));
    let recorded = lines(&root);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["level"], "error");
    assert_eq!(recorded[0]["process"], "tod-cli");
    assert!(recorded[0]["message"].as_str().unwrap().contains("unknown noun"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_successful_run_records_nothing() {
    let root = std::env::temp_dir().join(format!("tod-cli-problems-ok-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let out = run(&root, &["--build-stamp"]);
    assert!(out.status.success());
    assert!(lines(&root).is_empty());
    std::fs::remove_dir_all(&root).ok();
}
