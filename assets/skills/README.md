# Skills for working with tod

Read a skill with `tod-cli skills show <name>`. Each is for one job that
comes up rarely; use it when you are asked to do that job, or run into it and
do not know how.

| Skill | Use it when |
|---|---|
| `journeys` | You are asked to look at a journey (a node's, or the user's recent activity): how work on a node went, or what the user did. |
| `tod-open-pr` | You open a node's pull request, or bring its title and description up to date. |
| `tod-fix-ci` | A node's pull request has failing checks or review comments to answer. |

A skill may also be chosen for a lifecycle phase (the Lifecycle config
capability, by name); the phase's context then tells its agent to read it.

To add a skill, write `assets/skills/<name>.md` and list it above.
