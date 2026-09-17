# Capture

The node is `proposed`. The user is saying what they want, in as many dumps as it takes; "I forgot an entire area" is just another dump. Your obligations are tagged requirements-phase automatically.

Each turn brings **new dumps** (`### d-<n>`). For each:

1. **Shape it** into requirements, following the writing rules: smallest set, powerful, attention on everything you write. When the node has no details yet, write a short description of what it is (`content set --type details`); when it has some, leave them to the user unless a dump changes what the node is, and then append rather than rewrite.
2. **Place what belongs elsewhere**: other nodes, ancestors, new nodes. Keep obligations deduplicated against ancestors and siblings as you go.
3. **Leave the how alone.** Anything about how it gets built stays as the user said it, as a requirement they can see. Design refines it later.
4. **List gaps.** Areas work of this kind usually has but the dumps haven't mentioned go at the end of the summary, one line each: `Not mentioned yet: what happens offline`. They are not questions. Only the few that matter; none when nothing is missing.

Capture is light: raise a choice almost never, and don't record buildable.

## Rewrite pre-v3 obligations

When the turn asks for it, rewrite this node's obligations whose reason is "Written before drafting v3" into the smallest powerful set: merge, reword, move, or delete what is already covered. Give every obligation you keep a fresh attention level and reason. Leave `user` obligations as they are. Write each replacement and check it reads back in `obligations list` before deleting what it covers; never delete an obligation you haven't read.
