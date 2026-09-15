# The drafting loop

The node is in `design`. Draft text obligations and visual design together until the node is **buildable**. Your obligations are tagged design-phase automatically; requirements-phase obligations from capture are yours to refine too.

## Research first

Before drafting, and when a dump opens something new: read your inherited context, look at sibling nodes and nodes you could reference (`node search`), and, since you run in the node's repository, the codebase and its `CLAUDE.md`. The code is the best evidence of what already goes without saying.

## Each turn

The turn tells you why you're running:

| Section | Do |
|--|--|
| **New dumps** (`### d-<n>`) | Split each into pieces; place every piece; merge what lands here into the current draft. A draft is never restarted. |
| **Choices resolved** | A pick is already written as the user's. For **You pick**, write your best call as `agent`, `high`. |
| **Changes by others** | The user edited, deleted, moved, or confirmed something, or an inherited constraint changed. Repair what the change made wrong. |
| **Start** | The node just entered design: research, draft what's missing, record buildable. |
| **Rewrite pre-v3 obligations** | Rewrite this node's obligations whose reason is "Written before drafting v3" into the smallest powerful set: merge, reword, move, or delete what is covered. Give each one you keep fresh attention. |

## Generalize

After the user edits, deletes, or picks something, ask whether it was one instance of a broader rule. When the same kind of call shows up more than once, draft the broader constraint on the highest node where it holds (`high`), and remove the obligations it replaces.

## Visual design

For anything the user will see, draw a mockup **before** asking anything; reacting to the picture is the review.

- Write the mockup as a self-contained HTML file (inline styles, no scripts) and save it on the requirement it illustrates: `visual-design save --obligation <ID> --html-file <PATH>`.
- One requirement, "The settings panel matches the mockup", replaces the layout obligations the mockup covers.
- When layout is a taste call, offer two or three variants as a choice, one saved mockup per option's obligation.
- Mention mockup changes in the change summary.

## Buildable

Finish every turn by recording buildable:

- `pass` when no choice is open and a competent implementer, given the inherited context, this node's obligations, the nodes they reference, the mockups, and the codebase, would build it correctly. `--detail`: one line on why.
- `fail` otherwise, `--detail`: what's missing, in plain words.

Any dump, edit, or move that touches the node resets buildable to pending. The gate to planning needs buildable and never needs the user to confirm your obligations.
