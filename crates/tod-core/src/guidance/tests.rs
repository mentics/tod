use super::*;

fn skill(name: &str, text: &str) -> ProjectSkill {
    parse_skill(name, &format!("/w/.claude/skills/{name}/SKILL.md"), text).unwrap()
}

#[test]
fn a_skills_front_matter_gives_its_description_and_triggers() {
    let s = skill(
        "migrations",
        "---\nname: migrations\ndescription: >\n  How we write\n  migrations.\napplies_to:\n  paths: ['*.sql', 'crates/*/schema.rs']\n  terms: [ALTER TABLE]\n---\nNumber them.\n",
    );
    assert_eq!(s.description, "How we write migrations.");
    let triggers = s.triggers.unwrap();
    assert_eq!(triggers.paths, ["*.sql", "crates/*/schema.rs"]);
    assert_eq!(triggers.terms, ["ALTER TABLE"]);

    // No front matter, or none of the keys: no triggers.
    assert_eq!(skill("plain", "Just text.").triggers, None);
    assert_eq!(skill("empty", "---\ndescription: d\napplies_to: {}\n---\n").triggers, None);
}

#[test]
fn front_matter_that_is_not_yaml_keeps_its_description_but_never_hides_triggers() {
    // Agents read a loose description line; so does the selector's list.
    let loose = skill("loose", "---\ndescription: Use when: anything: really\n---\nBody");
    assert_eq!(loose.description, "Use when: anything: really");
    let err = parse_skill("bad", "/w/bad/SKILL.md", "---\ndescription: a: b: c\napplies_to:\n  paths: [x\n---\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("/w/bad/SKILL.md") && err.contains("applies_to"), "{err}");
}

#[test]
fn the_app_matches_triggers_by_changed_path_then_by_term() {
    let skills = vec![
        skill("migrations", "---\napplies_to:\n  paths: ['*.sql']\n---\n"),
        skill("locks", "---\napplies_to:\n  terms: [BEGIN IMMEDIATE]\n---\n"),
        skill("ui", "---\napplies_to:\n  paths: ['crates/tod-ui/*']\n---\n"),
        skill("style", "---\ndescription: no triggers\n---\n"),
    ];
    let files = vec!["db/001_add.sql".to_string(), "src/store.rs".to_string()];
    let matches = app_matches(&skills, &files, "+ conn.execute(\"begin immediate\")", false);
    let got: Vec<(&str, &str)> = matches.iter().map(|m| (m.item.as_str(), m.reason.as_str())).collect();
    assert_eq!(
        got,
        [
            ("migrations", "changed file `db/001_add.sql` matches `*.sql`"),
            ("locks", "the change mentions `BEGIN IMMEDIATE`"),
        ]
    );
    assert!(matches.iter().all(|m| m.kind == KIND_SKILL));
}

#[test]
fn before_a_change_the_design_names_the_paths_matched() {
    let skills = vec![skill("ui", "---\napplies_to:\n  paths: ['crates/tod-ui/*']\n---\n")];
    let design = "Add a column to `crates/tod-ui/src/unified/panels/task.rs`, then (maybe) docs.";
    let matches = app_matches(&skills, &named_paths(design), design, true);
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].reason,
        "the design names `crates/tod-ui/src/unified/panels/task.rs`, which matches `crates/tod-ui/*`"
    );
}

#[test]
fn areas_are_the_directories_of_the_changed_files() {
    let files: Vec<String> = ["a/b/c.rs", "a/b/d.rs", "top.md", "x/y.sql"].iter().map(|s| s.to_string()).collect();
    assert_eq!(areas(&files), [".", "a/b", "x"]);
}

#[test]
fn the_routing_table_keeps_the_lines_that_route_to_a_skill() {
    let survey = Survey {
        skills: vec![skill("sql-patterns", "")],
        docs: vec![(
            "/w/CLAUDE.md".to_string(),
            "# Rules\n| Writing SQL | `sql-patterns` |\n| Other | x |\n- see sql-patterns-old\n- use sql-patterns for queries\n"
                .to_string(),
        )],
        files: Vec::new(),
    };
    let table = routing_table(&survey);
    assert!(table.contains("From `/w/CLAUDE.md`"), "{table}");
    assert!(table.contains("| Writing SQL | `sql-patterns` |"), "{table}");
    assert!(table.contains("- use sql-patterns for queries"), "{table}");
    assert!(!table.contains("Other") && !table.contains("sql-patterns-old"), "{table}");
}

#[test]
fn the_survey_finds_the_projects_skills_nearest_first_and_the_files_that_exist() {
    let root = tod_test_util::scratch_dir(format!("tod-guidance-survey-{}", Uuid::new_v4()));
    let repo = root.join("ws").join("repo");
    let write = |path: PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(repo.join(".claude/skills/style/SKILL.md"), "---\ndescription: near\n---\n");
    write(root.join("ws/.claude/skills/style/SKILL.md"), "---\ndescription: far\n---\n");
    write(root.join("ws/.claude/skills/team/sql/SKILL.md"), "---\ndescription: grouped\n---\n");
    write(root.join("ws/CLAUDE.md"), "| SQL | sql |\n");
    write(repo.join("REVIEW.md"), "Review rules.");
    // The home directory's own are not the project's.
    write(root.join(".claude/skills/mine/SKILL.md"), "---\ndescription: home\n---\n");
    let survey = survey_host(&repo, Some(&root), &["REVIEW.md".to_string(), "sub/CLAUDE.md".to_string()]).unwrap();
    let names: Vec<(&str, &str)> = survey.skills.iter().map(|s| (s.name.as_str(), s.description.as_str())).collect();
    assert_eq!(names, [("style", "near"), ("sql", "grouped")]);
    assert_eq!(survey.files, ["REVIEW.md"]);
    assert_eq!(survey.docs.len(), 1);
    assert!(survey.docs[0].0.ends_with("ws/CLAUDE.md"));
}

#[test]
fn a_remote_surveys_records_parse_as_the_hosts_would() {
    let text = "\x1eskill /r/.claude/skills/a/SKILL.md\n---\ndescription: A\n---\nbody\n\
                \x1edoc /r/CLAUDE.md\n| x | a |\n\x1efile REVIEW.md\n";
    let survey = parse_survey(text).unwrap();
    assert_eq!(survey.skills[0].name, "a");
    assert_eq!(survey.skills[0].description, "A");
    assert_eq!(survey.docs, [("/r/CLAUDE.md".to_string(), "| x | a |\n".to_string())]);
    assert_eq!(survey.files, ["REVIEW.md"]);
}
