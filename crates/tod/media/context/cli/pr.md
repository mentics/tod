## `tod-cli pr`

Opening and driving a node's pull request. The app reads the pull requests
linked to the node and the status you record, not your reply.

```
tod-cli --data-root <DATA_ROOT> pr list                              [--node <NODE_UUID>] [--all-open]
tod-cli --data-root <DATA_ROOT> pr open                              [--node <NODE_UUID>] --owner <OWNER> --repo <REPO> --head <BRANCH> --base <BRANCH> --title <TEXT> [--body <TEXT>]
tod-cli --data-root <DATA_ROOT> pr status                            [--node <NODE_UUID>]
tod-cli --data-root <DATA_ROOT> pr comment reply <COMMENT_ID> <TEXT> [--node <NODE_UUID>] [--pr <LINK>]
tod-cli --data-root <DATA_ROOT> pr mergeable                         [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr merged                            [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> pr blocked                           --why <TEXT>
```

A node's pull requests are the links in its Ticket capability, which the user
edits too: the `pr → approved` and `approved → merged` gates check every one.
To link a pull request that already exists, add its URL there (`capabilities
set <NODE> ticket --pr <URL>`, which replaces the list, so pass every link);
`pr open` adds the one it opens.

Inside a `pr` conversation `--node` defaults to the node under work. `list`
shows every pull request, in any state, from the node's branch in each
repository its work spans — the one its Files capability names and each
submodule in it — grouped by repository; `--all-open` lists every open pull
request in those repositories instead, whichever branch it is from. `--json`
gives the same as data.
`open`
creates the pull request on GitHub (or finds the one already open from
`--head`) and links it to the node; it refuses when the node already links one
in that repository, so run `status` first if you are not sure one exists.
`status` fetches each linked PR's live mergeable flag, combined check status,
and merged flag — run it to see what changed since your last turn. `comment
reply` answers a review comment thread by its GitHub numeric id (shown in
`status` or in the comment itself, not tod's short ids); `--pr` (the PR's URL
or `<OWNER>/<REPO>#<NUMBER>`) says which PR, needed only when the node links
more than one.

`mergeable` records that the PR is ready — checks green, requested changes
addressed — for the `pr → approved` gate to confirm; it does not approve
anything itself. `merged` records the terminal state if the PR was merged out
of band. Both only work inside a `pr` conversation, and end your job: the
`pr → approved` and `approved → merged` gates are the app's own GitHub
checks, not yours to decide.

`blocked` records that you cannot make further progress without the user (an
unresolvable conflict, a requested change you cannot judge); the app hands
back to them with your `--why`.

`pr` needs no token from you. In an autonomous node's cloud sandbox GitHub is
signed in for every request (the sandbox's network adds the user's token),
so `pr`, `gh` (its `GH_TOKEN` is a placeholder that is replaced in flight),
and `git push` over HTTPS all work as the user; never try to find or set a
token there.
