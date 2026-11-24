# tod redesign

Working requirements for the redesign. The workspace should let a user define the product, direct people and agents doing the work, and improve the process using evidence from that work.

| Document | Covers |
|---|---|
| [Product definitions](product-definitions.md) | Durable descriptions of the product, requirements, constraints, and work that implements them |
| [Tags and relationships](tags-and-relationships.md) | Reusable obligations, custom relationships, and how their effects are explained |
| [Workbench](workbench.md) | Shared panels, queries, keyboard behavior, and chat |
| [Lifecycle workflows](lifecycles.md) | Workflow authoring, typed gate conditions, execution, and self-improvement |
| [Collection panel](collection-panel.md) | Collection config, inline chips, autosave, configurable row display, and collection pane interactions |
| [Self-improvement](self-improvement.md) | Lessons, analysis, process changes, metrics, and alerts |
| [Interactive prototype](prototype.md) | Mock data, keyboard bindings to try, and walkthroughs |

## Working in these files

- Give each requirement or constraint a stable ID, a short statement, and an observable acceptance condition. Use `REQ-` for an outcome, `CON-` for a rule that limits how work is done, and `WORK-` for a unit of execution. Reuse the ID when the title changes.
- Keep each item in the document that owns it. Link to its heading when a component, task, or decision refers to it. Add a separate file once an item requires enough detail to justify one.
- Record relationships in plain language with links to both endpoints. For example, “`WORK-QUERY-PROTOTYPE` exercises `REQ-QUERY-01`.” Tag assignments and the obligations they apply are explicit in the text.
- Treat entries here as working requirements. Mark disputed or unsettled behavior as **Open** next to the relevant entry. Track execution in linked work items so a completed task does not close the durable requirement.

The [current architecture](../architecture.md) and [unified view](../ui/unified-view.md) describe the existing application. This folder describes the intended redesign.

The [native ZGX frontend](../../frontends/tod-zgx/README.md#collection-panels) opens selected requirements from this directory as items. The Markdown files supply the initial requirement text.
