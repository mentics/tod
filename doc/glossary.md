# Glossary

## Items and collections

### Item

Anything a user can work with in tod. A task is the primary example. Product components, requirements, sessions, and rules can also be items. Any item can have tags.

### Item type

The kind of item, such as task or requirement. Each type has a distinct icon used consistently wherever that type appears.

### Tag

A label associated with an item. The user can add tags manually, and the app can assign them automatically. An item can carry several tags.

### Status

A named condition an item can show at a glance. A status has a label, an icon, and a color. Status and lifecycle phase are separate fields. The status vocabulary and how values are assigned are open design questions.

### Lifecycle phase

The step an item has reached in its assigned lifecycle, such as Plan or Review.

### Collection config

The definition of a collection. It specifies membership through search and filters or a list of explicitly chosen items. It can also specify order, grouping, and relationship-based nesting. A config can be unsaved or saved under a name. Applied changes to a named config autosave immediately. Panels, agents, and rules can use the same definition.

### Collection

The items and their arrangement when a collection config is evaluated. Query membership can change as data changes. A manual list keeps the user's chosen membership while showing current data for those items.

### Group by

Organize collection items under headings based on a shared value, such as item type.

### Nest by

Arrange collection items under other items using a chosen relationship or item-valued field, such as `Part of`.

### Layout slot

A place in the user's workspace layout that can host a panel. The user can arrange slots and choose which panel each holds.

### Panel

A group of UI components that occupies a layout slot. A panel can contain one or more panes.

### Pane

A subsection of a panel with a specific purpose.

### Collection panel

A panel with controls for the active config, a config widget, and a collection pane. Its header shows the config name or `<unsaved>`. Activating that label opens the config widget. The panel also holds presentation preferences such as column widths and which groups are expanded. See the [collection panel design](redesign/collection-panel.md).

### Collection pane

The part of the collection panel that displays the current collection and lets the user browse, mark, inspect, and act on its items.

## Current task model

**lifecycle config**
A capability that lists, per lifecycle phase, the skills the agent for that phase should use. It is set on a high-level node and inherited by everything below it; a lower node overrides a phase by listing it itself, and an empty list turns that phase's skills off.

**task node**
A node that is defined by having the *lifecycle* capability on itself and an *agent* capability that is inheritable. It is expected that one lifecycle processor is assigned to it to drive it to completion through the lifecycle phases.
