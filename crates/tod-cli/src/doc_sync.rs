//! Keeps the agent-facing `tod-cli` reference honest.
//!
//! `crates/tod/media/context/cli/<noun>.md` is what an agent is told this
//! binary can do; the `USAGE` string in each noun's module is what it can
//! actually do. Nothing links them at runtime, so they drift silently — add a
//! verb, and every agent keeps working from a reference that does not mention
//! it.
//!
//! The test below pins each fragment to its noun's own usage text. It lives in
//! `tod-cli` rather than `tod-core` because the usage strings are here; the
//! fragments are read from the source tree, not through `MediaPaths`.

/// Every noun, as `(noun, usage text)`. The fragment is
/// `media/context/cli/<noun>.md`.
#[cfg(test)]
pub(crate) fn nouns() -> Vec<(&'static str, &'static str)> {
    vec![
        ("node", crate::node::USAGE),
        ("obligations", crate::obligations::USAGE),
        ("plan", crate::plan::USAGE),
        ("visual-design", crate::visual_design::USAGE),
        ("content", crate::interview::CONTENT_USAGE),
        ("questions", crate::interview::QUESTIONS_USAGE),
        ("memory", crate::interview::MEMORY_USAGE),
        ("interview", crate::interview::INTERVIEW_USAGE),
        ("changeset", crate::changeset::USAGE),
        ("tests", crate::test_runs::USAGE),
        ("review", crate::review::USAGE),
        ("verdicts", crate::verdicts::USAGE),
        ("secrets", crate::secrets::USAGE),
    ]
}

/// The verbs listed under `COMMANDS:` — lines indented exactly four spaces and
/// starting with a lowercase word. Continuation lines are indented further and
/// prose is not indented at all, so both are skipped.
#[cfg(test)]
pub(crate) fn verbs(usage: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in usage.lines().skip_while(|l| !l.starts_with("COMMANDS:")) {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let verb = rest.split_whitespace().next().unwrap_or_default();
        if !verb.is_empty()
            && verb
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-')
            && !out.contains(&verb)
        {
            out.push(verb);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fragment(noun: &str) -> Option<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tod")
            .join("media")
            .join("context")
            .join("cli")
            .join(format!("{noun}.md"));
        std::fs::read_to_string(path).ok()
    }

    /// Every noun the binary dispatches has a fragment a surface can opt into.
    /// Without one, a recipe has no way to tell an agent the noun exists.
    #[test]
    fn every_noun_has_a_cli_fragment() {
        for (noun, _) in nouns() {
            assert!(
                fragment(noun).is_some(),
                "tod-cli dispatches `{noun}` but media/context/cli/{noun}.md does not exist"
            );
        }
    }

    /// The fragment must mention every verb the noun actually accepts.
    #[test]
    fn every_verb_is_documented_in_its_fragment() {
        for (noun, usage) in nouns() {
            let Some(doc) = fragment(noun) else { continue };
            for verb in verbs(usage) {
                assert!(
                    doc.contains(&format!("{noun} {verb}")),
                    "media/context/cli/{noun}.md never shows `{noun} {verb}`, \
                     but tod-cli accepts it"
                );
            }
        }
    }

    /// And must not invent verbs the binary would reject.
    #[test]
    fn fragments_do_not_document_verbs_that_do_not_exist() {
        for (noun, usage) in nouns() {
            let Some(doc) = fragment(noun) else { continue };
            let real = verbs(usage);
            // Only the invocation lines, never prose — "a top-level node with
            // no parent" is not a claim that `node with` exists.
            for line in doc.lines().filter(|l| l.starts_with("tod-cli ")) {
                let Some(rest) = line.split(&format!(" {noun} ")).nth(1) else {
                    continue;
                };
                let claimed = rest.split_whitespace().next().unwrap_or_default();
                if claimed.is_empty()
                    || !claimed.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                {
                    continue;
                }
                assert!(
                    real.contains(&claimed),
                    "media/context/cli/{noun}.md documents `{noun} {claimed}`, \
                     which tod-cli does not accept (has: {real:?})"
                );
            }
        }
    }

    /// Guards the extractor itself: a silently-empty verb list would make the
    /// two tests above pass for any fragment at all.
    #[test]
    fn the_verb_extractor_finds_verbs() {
        for (noun, usage) in nouns() {
            assert!(
                !verbs(usage).is_empty(),
                "no verbs parsed out of `{noun}`'s usage string"
            );
        }
    }
}
