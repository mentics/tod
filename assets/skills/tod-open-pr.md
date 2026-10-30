# Opening and refreshing a node's pull requests

How the `pr` phase gets a node's pull requests in place: one draft per
repository the change touches, each pushed and describing the change as it is
now. It starts where the work is already built, reviewed and committed:
getting there is the job of earlier phases.

This skill is the procedure. A project skill configured after it for the same
phase holds the project's own conventions (which repositories there are, the
title format, required description sections, how to attach screenshots);
where it says something more specific, follow it.

## Every time, not only the first

You run on every stay in `pr`, and you cannot tell from the PRs alone whether
this is the first time. A node moved back from a later state may already link
PRs, and since they were last checked the work may have changed a lot: new
commits, a different scope, a repository that now has changes and has no PR
yet, or one that no longer has any. The phase is certified only by you, in
this stay, so treat what you find as unchecked and bring all of it up to date.

## 1. What should exist

List every repository the node's branch changed: each one whose branch has
commits its base branch does not. In a workspace of submodules, each
submodule is its own repository, worked from inside it; the superproject is
one more when it records new submodule commits or changes files of its own.
Each of these needs exactly one open PR, from the node's branch to the
repository's default branch (unless the node says another base).

Push each branch first. Uncommitted work is not yours to leave behind:
commit what belongs to the change, and say in your reply if anything does not.

## 2. What exists

`tod-cli pr status` lists the PRs linked to the node and their state on
GitHub. A PR may also exist for a branch without being linked; `pr open`
finds it and links it rather than opening a second.

## 3. Make them match

For each repository:

- **No PR**: open one with `tod-cli pr open`, never `gh pr create`: `pr open`
  also links it to the node, which is how the app finds it to watch its
  checks and reviews.
- **An open PR**: rewrite its title and description for the change as it is
  now (`gh pr edit <pr> --title … --body …`). Describe the PR, not its
  history: no "updated to also …".
- **A closed, unmerged PR**: reopen it (`gh pr reopen`) if its branch is the
  node's branch; otherwise open a new one, link it, and unlink the old one.
- **A merged PR** with new commits on the branch since: open a new PR for
  them, link it, and unlink the merged one.

And for a linked PR whose repository no longer has changes, close it with a
comment saying why and unlink it (`tod-cli pr unlink`).

Every PR stays a **draft**: `pr open` opens one. If one was marked ready for
review, leave it; but do not mark any ready and do not request reviewers: a
person does both, and that is what the gate out of `pr_draft` waits for.

Run `gh` from inside the repository the PR belongs to. From the wrong one it
says "Could not resolve to a PullRequest", which reads like a broken PR.

## The title

Short and specific: what the change does, not how. When the node has an issue
in a tracker, the project skill says whether and how to name it.

## The description

A short summary, not a report:

1. **Bullets first**: up to 4–5, covering what was done and, where it is not
   obvious, why. Leave motivation out when the change speaks for itself.
2. **Prose after**: a sentence or two only if the bullets leave something
   worth clarifying. Many small PRs need none.
3. **The issue reference** on its own last line, when there is one and the
   tracker links PRs that way.

Write it for a reviewer who has not followed the work, from the node's
obligations and what was actually built. When a change spans several PRs,
each says what its own part does and names the others. Do not paste the
repository's PR template scaffolding, empty headings, or checklists; fill
only what carries real information. Shorter is better: reviewers can ask for
more.

The exception: a section that the phase or a configured skill requires (a
requirement-justification block, say) opens the body ahead of the bullets, in
full. Do not fold it into a bullet or trim it for brevity.

Do not mention AI, Claude, or an assistant anywhere in the PR.

## Screenshots

For a visible change, a screenshot helps the reviewer. GitHub has no public
API for attaching images, so how to upload one is the project's to say. If
there is no way to, or it fails, say so in your reply rather than leaving the
screenshot out silently.

## 4. Certify

When every repository that changed has exactly one open draft PR, linked,
pushed, and describing the change as it is now, and no linked PR is left
over, certify the phase: `tod-cli phase certify --note` with one line saying
which PRs you checked. That is what lets the node move on to `pr_draft`.

If you cannot get there (a branch you cannot push, a PR you cannot reopen),
do not certify: record `tod-cli pr blocked --why` and stop.

Checks and review feedback are not this phase's work: `pr_draft` deals with
them (`tod-fix-ci`). Stop once you have certified.
