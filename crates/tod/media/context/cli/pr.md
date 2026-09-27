## `tod-cli pr`

Opening and driving a node's pull request. The app reads the PR reference and
status you record, not your reply.

```
tod-cli --data-root <DATA_ROOT> pr list                              [--node <NODE_UUID>] [--all-open]
tod-cli --data-root <DATA_ROOT> pr open                              [--node <NODE_UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
tod-cli --data-root <DATA_ROOT> pr status                            [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr comment reply <COMMENT_ID> <TEXT> [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr mergeable                         [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr merged                            [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr blocked                           --why <TEXT>
```

Inside a `pr` conversation `--node` defaults to the node under work. `list`
shows every pull request, in any state, from the node's branch in each
repository its work spans — the one its Files capability names and each
submodule in it — grouped by repository; `--all-open` lists every open pull
request in those repositories instead, whichever branch it is from. `--json`
gives the same as data.
`open`
creates the pull request on GitHub and records it on the node — it only works
once per node; run `status` first if you are not sure one already exists.
`status` fetches the PR's live mergeable flag, combined check status, and
merged flag — run it to see what changed since your last turn. `comment
reply` answers a review comment thread by its GitHub numeric id (shown in
`status` or in the comment itself, not tod's short ids).

`mergeable` records that the PR is ready — checks green, requested changes
addressed — for the `pr → approved` gate to confirm; it does not approve
anything itself. `merged` records the terminal state if the PR was merged out
of band. Both only work inside a `pr` conversation, and end your job: the
`pr → approved` and `approved → merged` gates are the app's own GitHub
checks, not yours to decide.

`blocked` records that you cannot make further progress without the user (an
unresolvable conflict, a requested change you cannot judge); the app hands
back to them with your `--why`.
