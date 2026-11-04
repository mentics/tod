# Skills for working with tod

Read a skill with `tod-cli skills show <name>`. Each is for one job that
comes up rarely; use it when you are asked to do that job, or run into it and
do not know how.

| Skill | Use it when |
|---|---|
| `journeys` | You are asked to look at a journey (a node's, or the user's recent activity): how work on a node went, or what the user did. |
| `tod-open-pr` | You make sure a node has one up-to-date draft pull request per changed repository, and certify the `pr` phase. |
| `tod-fix-ci` | A node's pull request has failing checks or review comments to answer (`pr_draft`, `pr_review`). |
| `code-review-general` | You review a set of code changes (the `review` phase, or a review asked for directly): the method, checklists, severity and output. |

A skill may also be chosen for a lifecycle phase (the Lifecycle config
capability, by name); its text is then put in that phase agent's context.
`tod-open-pr` is meant for the `pr` phase, `tod-fix-ci` for `pr_draft` and
`pr_review`, and `code-review-general` for `review`. All are generic: a
project's own details (its repositories, required checks, review bots, title
format, review conventions) go in a skill of the project's own, listed after
tod's for the same phase.

To add a skill, write `assets/skills/<name>.md` and list it above.
