# tod

## Goal

Make it extremely efficient for a software engineer to work with many agents — on the order of a hundred simultaneously — with the ability to examine work at any granularity: high level, low level, and everywhere in between.

## Requirements

### Core model: tasks, agents, and association

1. Concurrent tasks and agents — Users manage tasks and agents as first-class units with a one-to-many association from tasks to agents (one task may have multiple agents; an agent belongs to at most one task). Association is established implicitly when an agent is launched for a specific task. Tasks are the user-specific work set; agents are the managed fleet associated with those tasks.
   - Success criteria:
     - tod can track/manage at least ~100 agents in the UI (agents may be idle)
     - tod can track/manage at least ~500 tasks in the UI (tasks may be idle)
     - At least ~10 local agents and ~10 micro-VM agents can run concurrently

2. Task lifecycle — Each tod task has a lifecycle state from this ordered set:
   proposed → design → planning → ready → active → verifying → review →
   approved → merged → released → learn → done.

   Tasks created manually or from a Linear issue start in the proposed lifecycle state.

   - Success criteria:
     - User can see a task’s current lifecycle state
     - User can change a task’s lifecycle state within that set

3. Agent runtime status — Each managed agent has a runtime status from this set:
   - Starting — launched; has not yet received a prompt
   - Processing — working on a prompt
   - Blocked — could not finish the prompt; needs human input to continue
   - Waiting — finished the prompt successfully; awaiting further instructions or shut down

   When the user shuts down an agent, tod tears down the agent and its environment and removes it from the managed fleet (no durable post-shutdown / retired state). Interrupting an agent halts its current activity and sets its runtime status to Blocked; the agent remains in the managed fleet. Shut down is the only operation that removes an agent from the fleet. When an agent is in Waiting or Blocked, the user can submit a new prompt without relaunching the agent. When an agent is in Processing, the user can submit a new prompt and choose whether it interrupts the current work or is added to the agent’s prompt queue.

4. Manual task create, edit, and delete — Users can create, edit, and permanently delete tasks manually in tod, including fields such as freeform notes, git repository, and branch. Title and slug are required; when the user does not supply a slug, tod auto-generates it from the title and from the linked issue ticket id when available (same rules as prior Tod extension). Slugs are editable after creation; when the linked issue ticket id is added or changed, tod auto-updates the slug from the title and ticket id unless the user has manually changed the slug. Titles must be unique across all tasks (case-insensitive); slugs must be unique across all tasks; all other task fields are optional. tod blocks permanent delete of a task while it still has associated agents. When permanently deleting a task with no associated agents, tod reclaims any unreclaimed isolated worktrees for that task (subject to the dirty-worktree warning in item 14). The user can also explicitly reclaim an agent’s isolated git worktree.

5. Tags — Users can assign tags to tasks.
   - Success criteria:
     - User can add, remove, and view tags on a task

### UI: awareness, lists, and detail pages

6. Situational awareness — tod provides integrated task management and agent management UIs.
   - Success criteria:
     - Aggregate fleet status and per-agent and per-task status are visible at a glance
     - Each agent’s environment type (local, devcontainer, or Micro-VM) is visible in the UI
     - Integrated task management and agent management UIs are available
     - Tasks and agents can be sorted, filtered, and grouped; tasks can be sorted, filtered, and grouped by tags

7. Fuzzy search on lists — Anywhere there is a primary list in the UI, tod supports smart fuzzy search. Examples include tasks, agents, and notifications.

8. Agent detail page — Each managed agent has an agent detail page. The associated task and the agent’s current runtime status are visible on the agent detail page.

9. Task detail page — Each task has a task detail page for viewing and editing task fields. Associated agents are visible on the task detail page.

10. Status area — tod provides a status area that displays text about in-progress operations in response to user requests (such as a spinner while work is underway).

11. Operation failure feedback — When a user-requested operation fails, tod presents the error in a toast or banner, clearly more visible than the status area. **Confirmation toasts:** when an operation requires explicit consent before proceeding (e.g. setting up missing interview scaffolding), use a non-autohide notification with **No** and **Yes** actions; copy names what is missing. **No** dismisses and cancels; **Yes** proceeds. Shared helper: `crates/tod/src/ui/toast.rs` (`confirm_toast`).

### Agent operations and environments

12. Agents and environments — tod can manage agents in the following environments:
   1. Local (host)
   2. devcontainer
   3. Micro-VM

   For devcontainer agents, tod supports launching into an existing devcontainer the user already has open.

   Every agent launch is for a specific task.

   At launch, the user chooses the environment type. tod provides a control for the current default environment type; launches use that default. The default may be a specific environment type or ask every time — when set to ask every time, or when no default is set, tod prompts the user to choose the environment type at launch. Launching an agent does not require an initial prompt; the user may submit a prompt later.

   The following operations are supported (what the user can do):
   - Launch an agent
   - Review an agent’s status
   - Submit a prompt to an agent
   - View an agent’s transcript
   - Interrupt an agent
   - Switch a running agent between autonomous and interactive mode
   - Shut down an agent

   Viewing an agent’s transcript shows the full history of prompts and responses for that agent session.

13. Isolated worktrees for local and devcontainer agents — For local agents and devcontainer agents, tod provides each agent its own isolated git worktree to work in. Launching a local or devcontainer agent is blocked until the associated task has a git repository set. When creating a worktree, tod uses the associated task’s git repository and branch when those fields are set on the task. When the user shuts down the agent, tod reclaims that worktree (subject to the dirty-worktree warning in item 14). The user can also explicitly reclaim an agent’s isolated git worktree.

14. Dirty worktree warning on reclaim — When reclaiming an isolated git worktree, tod warns if the worktree has uncommitted changes and offers options before proceeding.

15. Launch shell into agent environment — Users can launch a shell into an agent’s environment to run commands and view the filesystem. Multiple concurrent shell sessions into the same agent’s environment are supported. tod tracks open shell sessions per agent so the user can switch among them; open shell sessions are visible on the agent detail page. Open shell sessions survive tod application restarts.

16. Agent autonomous and interactive modes —
   - Autonomous mode — the agent proceeds without per-step user approval until it finishes, is interrupted, or is blocked and cannot resolve the block on its own.
   - Interactive mode — the agent waits for user input before each step.

   At launch, the user chooses autonomous or interactive mode. tod provides a control for the current default mode; launches use that default. The default may be autonomous, interactive, or ask every time — when set to ask every time, or when no default is set, tod prompts the user to choose the mode at launch.

   When the user switches a running agent between autonomous and interactive mode, the new mode takes effect in a non-disruptive way — it does not interrupt the agent’s current work and applies as soon as it makes sense.

### Human-in-the-loop

17. Human-in-the-loop — When an agent needs help, it can reach the user through a managed notification queue in tod.
   - Success criteria:
     - tod maintains a managed notification queue when agents need human input or hit blockers
     - When an agent enters Blocked status, tod automatically adds a notification to the managed queue
     - Notifications include the agent’s question or blocker message text
     - Notifications persist until acted upon and resolved
     - From a notification, the user can see the related task, involved agents, and respond
     - From a notification, the user can open the related agent detail page
     - From a notification, the user can open the related task detail page
     - From a notification, the user can submit a prompt to the related agent through tod

### External integrations

18. Slack integration — Users can, on demand, read Slack content; tod fetches it to use in the user’s current action:
   1. Paste a Slack URL to a channel, thread, or message
   2. Paste a Slack channel name

19. Issue tracker integration — Issue-tracker integrations share a common capability set. Linear is the required issue tracker.

   From tod, the user can:
   1. Associate a tod task with one or more linked issues
   2. Create a tod task from an issue ticket ID (tod pulls the issue content and creates the task)
   3. Open a browser to an associated issue

20. Code repository integration — Code-repository integrations share a common capability set. GitHub is the required code repository.

   From tod, the user can:
   1. Associate a tod task with one or more linked pull requests
   2. Open a browser to an associated pull request

21. Credential management — The user can store, update, and replace credentials tod needs to access configured external services (Slack, Linear, and GitHub). Credentials live on the user’s machine. When credentials are missing or invalid for an action that needs them, tod prompts the user to supply them.
   - Success criteria:
     - User can set and change credentials for Slack, Linear, and GitHub used by tod
     - An action that needs a missing/invalid credential prompts for credentials rather than failing silently

22. Code editor integration — From tod, the user can open Zed to view the code a particular agent is working on (one supported editor in this phase; user configuration of editor choice is not required):
   1. Open that agent’s worktree, workspace, or branch
   2. Open or focus a specific file (and optionally a line)

   Note: showing an agent session’s changeset/diffs inside the code editor is deferred to a design spike (feasibility unknown); not required by this item.

### Data and persistence

23. Fleet state persistence — Tasks, agents, agent transcripts, open notifications, and open shell sessions into agent environments survive tod application restarts. All durable state is stored on the user’s machine and written when mutated (no separate explicit “save” required for ordinary mutations).
   - Success criteria:
     - After quit and relaunch, previously present tasks, agents, agent transcripts, unresolved notifications, and open shell sessions are still available
     - Ordinary mutations remain after relaunch without an explicit save step

24. Data import — Tod can import tasks and related data from a JSON dump. Tod defines a JSON schema for the input it supports and interprets input leniently—doing its best to make sense of the data provided.
   - Success criteria:
     - User can import from a JSON dump against tod’s documented import schema
     - When the dump is incomplete or imperfect, tod still imports the records and fields it can interpret (partial success allowed)

### Application platform

25. Application resource limits — The tod application itself stays within fixed resource bounds on the user’s machine (separate from agent or vendor workload limits).
   - Success criteria:
     - Application RAM stays under 500 MB
     - Application CPU stays under 2% when idle (doing nothing)
     - Application CPU stays under 5% under normal use

26. Application settings — Users can view and change application preferences in tod. Preferences are stored on the user’s machine and are separate from credentials.
   - Success criteria:
     - User can open settings and change a preference
     - Preference changes remain after quit and relaunch

27. Diagnostic logging — Users can view tod’s own diagnostic logs for troubleshooting.

### Interaction and safety

28. Keyboard efficiency — Every user action in tod is reachable via the keyboard; a user can operate tod fully without using the mouse.

29. Customizable keyboard shortcuts — Users can customize keyboard shortcuts in tod’s application preferences.

30. Destructive-action confirmation — tod requires user confirmation before destructive actions.
   - Success criteria:
     - User must confirm before permanently deleting a task
     - User must confirm before shutting down an agent that is in Starting, Processing, or Blocked
     - Shutting down an agent in Waiting does not require confirmation
     - Before shutting down an agent, if the agent’s working set has changes, tod warns the user and offers recovery options similar to dirty worktree reclaim before proceeding

## Constraints

1. Runs as a local desktop application on the user’s machine.

2. Supports Windows, macOS, and Linux.

3. Tod is for one user on their machine (no multi-user shared fleet).

4. Tod’s durable state lives on the user’s machine under a known local location the user can copy or back up with ordinary filesystem tools (no required cloud store for core state).

5. Logging practices — Follow [`doc/process/shared/constraints/logging-constraints.md`](../../shared/constraints/logging-constraints.md).

6. Selectable data — Follow [`doc/process/shared/constraints/selectable-data-constraints.md`](../../shared/constraints/selectable-data-constraints.md).

7. Resizable dividers — Follow [`doc/process/shared/constraints/resizable-dividers-constraints.md`](../../shared/constraints/resizable-dividers-constraints.md).
