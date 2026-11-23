# Product definitions

## Product items and work items

A **product item** describes something about the product that should persist across releases. Examples include a feature, a reusable component, a workflow, or an interaction pattern. A product item can describe how it looks and behaves, carry obligations, and link to design references, other product items, and evidence of conformance.

A **work item** describes a change someone can carry out. It can be imported or created locally, assigned to a person or agent, run through a lifecycle, and completed. Multiple work items can implement or verify one product item over time. The product definition preserves its own history and remains available for future work.

Both kinds of item can be found, tagged, grouped, inspected, and discussed through the same workbench. Their inspectors show the actions and fields appropriate to each kind.

## REQ-PRODUCT-01 · Trace a product definition to the work

A user can open a product item and find its direct and effective obligations, work in progress, completed implementation work, and verification evidence. From a work item the user can follow the same links back to the product definition and see the version relevant to that work.

**Acceptance** — Open **Query bar**, follow an `Exercised by` link to the prototype task, and return to the product definition from the task using `Exercises`. Completion of the task preserves the product item's requirements and links.

## REQ-OBLIGATIONS-01 · Manage requirements and constraints

A **requirement** describes an outcome to deliver or demonstrate. A **constraint** limits how that outcome can be achieved. Both are obligations. Each can be edited, reused, linked to evidence, and inspected from an item it applies to. A product item or task may own an obligation directly or receive it through a tag effect. [Tag effects](tags-and-relationships.md#req-tag-01--explain-effective-obligations) explain how it was applied.

**Acceptance** — Apply **Keyboard-first** to **Query bar**. Its inspector shows the keyboard constraint, the originating tag, the product item's direct requirements, and links to any evidence. When one obligation reaches the item through multiple paths, the inspector shows it once with each source available.

**Open** — When a work item implements a product item, should it receive linked obligations automatically, or should the user choose which ones apply to that change? Start by showing the product obligations in the work item and making any effect on its gates explicit.

## CON-KEYBOARD-01 · Operate every component by keyboard

Core operations can be completed with the keyboard, including selecting rows, opening actions, editing criteria, inspecting linked items, and navigating between panels. Controls show visible focus. Closing a transient panel or menu restores focus to its origin. Navigation shortcuts respect editable text fields.

**Acceptance** — Complete both [prototype walkthroughs](prototype.md#walkthroughs) using only the keyboard and identify which control has focus at each step.

## REQ-SPEC-01 · Manage tod's own specifications in tod

Product components, requirements, and constraints can describe tod itself. Give each specification a stable identity, editable text, and links to its implementation work and verification evidence. A revision preserves the identity and history so someone can see which version a task implemented.

**Acceptance** — Find `REQ-COLLECTION-01` in a collection, open its text, revise it, and follow its link to the work that implements it. The previous revision remains available from the same requirement.

The native ZGX frontend uses the existing Markdown as the starting text for `REQ-COLLECTION-01`. Its inspector saves local revisions and links to the implementation task. [Native frontend](../../frontends/tod-zgx/README.md#collection-panels)

## Applying this to the redesign today

The [Query bar](workbench.md#req-query-01--compose-and-reuse-a-query) is a product component. It owns `REQ-QUERY-01`. It carries the **Keyboard-first** tag, which applies [CON-KEYBOARD-01](#con-keyboard-01--operate-every-component-by-keyboard). [WORK-QUERY-PROTOTYPE](prototype.md#work-query-prototype--exercise-the-query-bar) exercises the requirement. This is an example of the same product, obligation, tag, and task links we can maintain in Markdown and eventually manage through tod.
