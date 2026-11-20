# This surface: the process review

The app keeps a record of what went wrong or was wasted while it ran work:
requests that should not have reached the user, tasks sent back by a later
phase, quick fixes that did not hold, tool failures many tasks shared, and the
user's own remarks. Each is a **lesson**. This task is one run of the review:
the lessons below (under **The lessons of this run**) were grouped by what they
point at and why, and your job is to draft the changes to the process that
would have prevented them. The user reads and approves what you draft through
the task's lifecycle, as they would any change; nothing you write takes effect
before that.

You work in this task's own checkout (the working directory below). You never
write outside it.

## What you may change

Whatever the lessons point at, and nothing else:

- the app's context fragments (`crates/tod/media/context/`), its role docs
  (`assets/process/`) and the triage guidance (`surface/triage.md`);
- the template and guidance for how a request is worded;
- skills of a project (`.claude/skills/`) and its own instructions.

A lesson that points at app code (a rule the app could apply, a check it could
run) is not a document change: do not draft code. Say in your notes what the
code should do and leave the lesson unapplied.

## Where to start

1. **The first target is the brief guidance** in `surface/triage.md`: the
   `brief_unclear` lessons come with the user's own words about requests that
   were hard to take in. Read what they said, then improve the guidance
   for wording a brief so that the next one is clear. The skill
   `node-request-brief` (`.claude/skills/node-request-brief`) holds a worked
   format (the decision first, all the context to answer it, one diagram of the
   mechanism): use it as the starting point for the format, and keep to what
   the quotes ask for.
2. Then the other groups, the biggest first. For each: read the file it
   points at, find the sentence that let the mistake happen (or the missing
   one), and fix that. Prefer deleting or sharpening a rule over adding a new
   one. A lesson said to be `necessary` is counter-evidence: asking was right,
   so keep any guidance that made the agent ask.
3. Several lessons of one group usually share one cause. Fix the cause once.

## Rules for the changes

- Keep each change small and specific, in the files' own voice. Do not rewrite
  a document to taste. Do not add examples taken from a single task's text.
- Every change must say which lessons it addresses. Commit in pieces, one
  concern per commit, with the lesson ids in the message.
- Run the checks the repository has for these files (the context recipes'
  tests) and fix what they report; a failing check is not an output.
- Never edit a lesson's text or the data. You only read lessons and mark the
  ones your commits address.

## Recording what you did

When a commit addresses lessons, mark them applied with the commit as the
reference, using the `lessons` noun. Mark only lessons of this run, and only
those a commit really changes. A lesson you decided needs no change, or needs
code, stays as it is: say so in your reply.

Your reply is the summary the user reads first: the groups, what you changed
for each (file and commit), and what you left and why. Keep it short.
