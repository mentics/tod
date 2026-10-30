# Opening a pull request

How a node's pull request is opened and described. It starts where the work
is already built, reviewed, committed and pushed: getting there is the job of
earlier phases and is not covered here.

## Opening it

Open it with `tod-cli pr open`, never `gh pr create`: `pr open` also links the
PR to the node, which is how the app finds it to watch its checks and reviews.
Check `tod-cli pr status` first; an earlier session may already have opened
it, and an existing PR for the branch is linked, not opened again.

- **Always a draft.** `pr open` opens one. Do not mark it ready for review and
  do not request reviewers: a person does both, and that is what the gate
  out of `pr_draft` waits for.
- **Base** is the repository's default branch (`main`) unless the node says
  otherwise. **Head** is the branch you pushed.
- **One PR per repository that changed.** In a workspace of submodules
  (`flagship`, `ui`, `kb`), the PR belongs to the submodule's own repository,
  opened from inside it; a change across two submodules is two PRs.

## The title

Short and specific: what the change does, not how. When the node has a Linear
issue, start with its ID: `ENG-1427: Show part context on the detail page`.
Otherwise no prefix.

## The description

A short summary, not a report:

1. **Bullets first**: up to 4–5, covering what was done and, where it is not
   obvious, why. Leave motivation out when the change speaks for itself.
2. **Prose after**: a sentence or two only if the bullets leave something
   worth clarifying. Many small PRs need none.
3. **The Linear ID** on its own last line, when there is one (`ENG-1427`), so
   Linear links the PR.

Write it for a reviewer who has not followed the work, from the node's
obligations and what was actually built. Do not paste the repository's PR
template scaffolding, empty headings, or checklists; fill only what carries
real information. Shorter is better: reviewers can ask for more.

The exception: a section that the phase or a configured skill requires (a
requirement-justification block, say) opens the body ahead of the bullets, in
full. Do not fold it into a bullet or trim it for brevity.

Do not mention AI, Claude, or an assistant anywhere in the PR.

## Keeping it current

When the PR already exists and what is built has moved on, bring the title and
description up to date with the change as it is now (`gh pr edit <pr> --title
… --body …`). Describe the PR, not its history: no "updated to also …".

## Screenshots and images

For a visible change, a screenshot helps the reviewer. GitHub has no public
API for image attachments, and on a private repository only a
`https://github.com/user-attachments/assets/<uuid>` URL renders inline. Where
`gh-attach` is on the `PATH` (the Cofactr workspace has it), it uploads
through the endpoint the web UI uses, with your `gh` token:

```
gh-attach shot.png                          # repository from the cwd's git remote
gh-attach before.png after.png --repo Cofactr/ui
# → ![shot.png](https://github.com/user-attachments/assets/<uuid>)

gh pr comment <pr> --body "What this shows:

$(gh-attach shot.png)"
```

- Markdown goes to stdout, diagnostics to stderr; one line per file, and the
  exit code says whether any failed.
- Supported: `png jpg jpeg gif webp svg mp4 mov webm`, images up to 10MB.
  Videos print as a bare URL, which GitHub renders as a player.
- Run it from the target repository, or pass `--repo`: assets belong to the
  repository they are uploaded to, and the token needs push access there. A
  permission failure comes back as **404**, not 403; `gh-attach check`
  uploads a 1x1 PNG to tell.

If `gh-attach` is not there, or fails, say so in your reply rather than
leaving the screenshot out silently.

## After opening

Stop there. The app watches the PR's checks and reviews and sends you back
when something needs doing; `tod-fix-ci` covers that work.
