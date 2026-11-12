---
name: node-request-brief
description: Turn a tod node's open request to the user into a published Artifact brief that has the decision first, all the context needed to answer it, and a diagram of the mechanism, so the user can answer in a minute without opening anything else. Use when a node is waiting on a decision only the user can make, or when the user asks for a brief, explainer or artifact about a node's request.
user_invocable: true
---

A brief answers one question: **what must the user decide, and what do they
need in order to decide it?** The user reads it instead of the node's
transcript, its obligations and the PR. Make one brief per node and publish it
as an Artifact; don't put several nodes on one page.

Only write a brief for a request that truly needs the user. Before writing,
check whether the request is about access, configuration or a broken setup
that you could resolve, or is something tod should have decided itself. If
so, it doesn't need a brief.

## Gather (read-only)

Get the data root from `install.toml`. Read it, never write it.

- **The request:** its exact text and options, with `tod-cli decisions list --node <uuid>` and `decisions show <id>` for
  the node, or the task panel. Note how long it has been open.
- **The node:**
  - `tod-cli node show <slug>`: title, state, Linear ticket;
  - the obligations the request touches, with their short ids and text;
  - plan steps and review findings that bear on it.
- **Evidence:** read what the agent cited, such as findings, queries and
  numbers. Check any number you repeat against its source. Don't repeat a
  claim you cannot check; say it is the agent's.
- **The PR,** if there is one: `gh pr view <n> -R <owner/repo>` for state,
  mergeability, checks, size and files touched.
- **Other nodes:** whether another node touches the same files or obligations,
  and so would conflict with this answer.

## Structure, in this order

1. **Header:**
   - ticket id;
   - lifecycle state;
   - how long it has waited;
   - links to the ticket and the PR;
   - the node title as the `h1`.
2. **The ask:** a bordered box, first on the page.
   - The question in one sentence, as an imperative or a direct question.
   - The options, numbered as they are in tod. Each has a short title and
     what it changes, such as the obligation by its short id or the code
     path. Each also has its result: what happens afterwards, with the
     trade-off stated plainly.
   - The recommended option comes first, marked "recommended", with one line
     on why.
   - Say if options can be combined (for example "1 and 2").
   - **How to answer:** select the node in tod, then press the option's
     number in the task panel or type an answer. Alt+Q jumps to the next node
     waiting on the user.
   - If nothing is to be answered yet (the user acts later), say what to do
     and when.
3. **The diagram:** an inline SVG of the real mechanism behind the question,
   such as the decision flow, the timeline, or the data path, with the failing
   part marked. Use a concrete example from the evidence (a real id or part
   number), with a caption of one or two sentences. Load the
   `artifact-diagramming` skill for this.
4. **Obligations involved:** each with its short id, its kind, and its text
   in plain words, plus what each option would change in it.
5. **Evidence:** what the review or agent found, with numbers, as a short
   list.
6. **Overlaps:** other nodes or PRs this answer affects, in a highlighted
   note.

## Writing

- Plain words. Explain every id the first time it appears.
- No transcript dumps, no tool output, no long quotes.
- Say "the agent says" for claims you did not check.
- Keep each option to a few lines, and keep the page to one screen of
  decision plus the supporting context below it.

## Publishing

- Load `artifact-design` first (the theme tokens, light and dark, a 16 px
  gutter at phone width). Load `artifact-diagramming` for the SVG.
- Write the file in the scratchpad as `briefs/<ticket>.html`, with a title
  like `ENG-1234 decision` and the icon `question`.
- Publish it with the Artifact tool, and give the user the link with a
  one-line summary of the question.
- When the request is answered or changes, republish the same file. Don't
  create a new link.
