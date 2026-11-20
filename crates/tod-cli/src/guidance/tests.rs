use crate::run_with;
use std::path::Path;

fn cli(root: &Path, env: &[(&str, &str)], args: &[&str]) -> anyhow::Result<String> {
    let pairs: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let lookup = move |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let mut full = vec!["--data-root".to_string(), root.display().to_string()];
    full.extend(args.iter().map(|a| a.to_string()));
    run_with(&full, &lookup)
}

#[test]
fn the_user_adds_lists_and_removes_and_an_agent_session_is_refused() {
    let (root, node, _) = crate::tests::data_root();
    let id = node.to_string();
    assert_eq!(cli(&root, &[], &["guidance", "list", &id]).unwrap(), "(no guidance selected)");
    cli(&root, &[], &["guidance", "add", &id, "--kind", "skill", "--item", "style"]).unwrap();
    cli(&root, &[], &["guidance", "select", &id, "--kind", "file", "--item", "REVIEW.md", "--reason", "Always."])
        .unwrap();
    let listed = cli(&root, &[], &["guidance", "list", &id]).unwrap();
    assert!(listed.contains("skill style — Added by the user. (by you,"), "{listed}");
    assert!(listed.contains("file REVIEW.md — Always. (by you,"), "{listed}");

    // An agent session may neither add nor remove, nor select outside a
    // selector run.
    let agent = [("TOD_INTERVIEW_ACTOR", "conversation:00000000-0000-0000-0000-000000000001")];
    for args in [
        &["guidance", "add", &id, "--kind", "skill", "--item", "x"][..],
        &["guidance", "remove", &id, "--kind", "skill", "--item", "style"],
    ] {
        let err = cli(&root, &agent, args).unwrap_err().to_string();
        assert!(err.contains("refused in an agent session"), "{err}");
    }
    let err = cli(&root, &agent, &["guidance", "select", &id, "--kind", "skill", "--item", "x", "--reason", "r"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("guidance"), "{err}");

    cli(&root, &[], &["guidance", "remove", &id, "--kind", "skill", "--item", "style"]).unwrap();
    let listed = cli(&root, &[], &["guidance", "list", &id]).unwrap();
    assert!(!listed.contains("style"), "{listed}");
    let all = cli(&root, &[], &["guidance", "list", &id, "--all"]).unwrap();
    assert!(all.contains("skill style") && all.contains("[removed by user]"), "{all}");
    let json: serde_json::Value =
        serde_json::from_str(&cli(&root, &[], &["--json", "guidance", "list", &id]).unwrap()).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
}
