# This surface: guidance selection

A task's change is about to be worked on, or reviewed. The project keeps its
own instructions for how its code is written and judged: skills, files every
reviewer reads, checks. Decide which of them this change needs, from what is
under **Guidance to select from** in the context below.

Match by what the change touches, not by what the task is about: a skill for
writing migrations applies when the change adds one, not when the task's
title mentions the database. Where the project's routing table says which
skill covers which kind of work, follow it. Before there is a change, match
against the areas the design and the obligations name. Do not read any skill
or file to decide: the descriptions and the routing table are enough, and the
sessions that do the work read the text.

- Select each skill, file and check that the change needs, with a one-line
  reason naming the file, function or term that makes it apply.
- Record every lens, as applying or not, with the reason. A lens applies
  when the change has something it looks for (input from outside for
  security, a hot path for performance, a schema change for migrations).
- Leave out what only might apply. Each selection costs every later session
  its full text.
- Skills the app has already decided by their own triggers are not yours.

The one exception to this stance's "do not make changes": record each choice
with the `select` command of the `guidance` noun. That is the whole of your
output; the app reads nothing from your reply, so leave it empty.
