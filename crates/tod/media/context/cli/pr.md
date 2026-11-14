## `tod-cli pr`

A node's pull requests (links in its Ticket capability; `pr open` adds one).
`--node` defaults to the node under work; `--pr` is a URL or
`<OWNER>/<REPO>#<NUMBER>`, needed only when the node links several.

```
tod-cli --data-root <DATA_ROOT> pr list                              [--node <NODE_UUID>] [--all-open]
tod-cli --data-root <DATA_ROOT> pr open                              [--node <NODE_UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
tod-cli --data-root <DATA_ROOT> pr link <LINK>                       [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr unlink <LINK>                     [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr status                            [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr comment reply <COMMENT_ID> <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr threads                           [--node <NODE_UUID>] [--pr <LINK>] [--all]
tod-cli --data-root <DATA_ROOT> pr threads answer <THREAD_ID>        (--fixed | --rejected) --reply <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr checks                            [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr checks unrelated <CHECK>          --reason <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr mergeable                         [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr merged                            [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr blocked                           --why <TEXT>
```

- `open` always opens a draft PR, and refuses when a PR is already linked in that repository; run `status` first.
- `link` attaches an existing PR to the node (a URL or `<OWNER>/<REPO>#<NUMBER>`);
  `unlink` removes one and keeps the others. To swap a PR, link the new one and
  unlink the old.
- `threads` lists open review threads, human and bot, with code, comments and
  roughly how many rounds each has had. `threads answer` posts `--reply` (what
  you did, or why not) and resolves the thread. `--fixed`: push first
  so the reply can name the commit. `--rejected`: you changed nothing.
- `checks` lists each linked PR's failing checks by name, with where the user's decision stands for any you called unrelated. `checks unrelated` says a failing check is not caused by this change; `--reason` is your evidence (the file or log line, and where else it fails: the base branch, other branches). The app takes that check off your list and asks the user once whether to fix it in this PR or carry on without it, so end your turn after it. It refuses a check that is not failing, and one the user decided to fix in this PR.
- `blocked --why` hands back to the user when you cannot proceed.
- `mergeable` and `merged` only record; the gates are the app's own checks.

No token is needed: `pr`, `gh` and `git push` already act as the user.
