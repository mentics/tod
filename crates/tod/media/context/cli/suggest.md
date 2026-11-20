## `tod-cli suggest`

Out-of-scope work worth doing, most often a follow-up ticket: something you
found that this task should not take on. It is a **suggestion**: it never
stops you or the task, and it is not a question. The user accepts it (the
ticket is created exactly as you wrote it) or dismisses it whenever it suits
them. Never ask a decision "should we open a follow-up ticket?"; file one.

```
tod-cli --data-root <DATA_ROOT> suggest ticket [--node <NODE>] --title <TEXT> --body <TEXT>
tod-cli --data-root <DATA_ROOT> suggest list   [--node <NODE>] [--all]
```

`--node` is the task's slug or full UUID; it defaults to the task you are
working on.

`ticket` files the suggestion. `--title` and `--body` are the ticket
exactly as it will be created, so write them to stand alone: the body is its
description (what and why, where in the code, what you saw) followed by its
acceptance criteria. Use `--body -` and a heredoc for anything long. Tickets
already in the tracker that look like it are searched for and shown to the
user with it; you need not search. An identical open suggestion is not filed
twice.

`list` shows the task's open suggestions, with the possible duplicates found
for each (`--all` adds the ones already accepted or dismissed). Look before
filing something you may have filed before.

File a suggestion only for work that is out of this task's scope and worth a
ticket of its own, not for what this task must do, and not for a nit.
