# The outline

This is a desktop application for turning conversations into structured
work. It keeps a hierarchical **outline** of tasks. A task is a unit of work —
a project, a feature, or a single change — and tasks nest inside one another.

Every task has a stable, unique **slug** shown alongside its title, and a UUID.
Either addresses the task. Prefer the slug once you know it: it survives a
rename, where a title does not.

The command line calls a task a *node* (`tod-cli node …`, `--node`). Use that
word only when typing a command or reading its output; everywhere else say
"task".

A task may carry freeform **details** describing it, written by the user or
an agent.

A task inherits context from its ancestors: each Spec ancestor's summary and
constraints. What is settled on a parent bounds what its children may do, and
a child's work is understood as serving its parent's scope.
