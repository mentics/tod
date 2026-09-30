## `tod-cli pr`

A node's pull requests (links in its Ticket capability; `pr open` adds one).
`--node` defaults to the node under work; `--pr` is a URL or
`<OWNER>/<REPO>#<NUMBER>`, needed only when the node links several.

```
tod-cli --data-root <DATA_ROOT> pr list                              [--node <NODE_UUID>] [--all-open]
tod-cli --data-root <DATA_ROOT> pr open                              [--node <NODE_UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
tod-cli --data-root <DATA_ROOT> pr status                            [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr comment reply <COMMENT_ID> <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr threads                           [--node <NODE_UUID>] [--pr <LINK>] [--all]
tod-cli --data-root <DATA_ROOT> pr threads answer <THREAD_ID>        (--fixed | --rejected) --reply <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr mergeable                         [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr merged                            [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr blocked                           --why <TEXT>
```

- `open` refuses when a PR is already linked in that repository; run `status` first.
- `threads` lists open review threads, human and bot, with code, comments and
  how often you answered. `threads answer` posts `--reply` (what you did, or
  why not), signs it as tod's, and resolves the thread. `--fixed`: push first
  so the reply can name the commit. `--rejected`: you changed nothing.
- `blocked --why` hands back to the user when you cannot proceed.
- `mergeable` and `merged` only record; the gates are the app's own checks.

No token is needed: `pr`, `gh` and `git push` already act as the user.
