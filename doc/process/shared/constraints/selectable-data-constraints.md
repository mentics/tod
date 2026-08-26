# Selectable data constraints

Reusable UI practice for desktop applications. Projects that adopt this document treat it as a binding constraint.

## Rule

All **displayed textual data** must be selectable with standard mouse-drag text selection so the user can copy it to the clipboard.

**Data** means content that came from somewhere other than fixed UI chrome — for example settings values, prompts, questions, answers, transcripts, notifications, error details, imported or linked external content, and any other user- or system-generated text shown in the UI.

## Exemptions

Static UI chrome does **not** need to be selectable. Examples include:

- Button labels
- Tab titles
- Section headers and field labels
- Navigation items
- Placeholder or helper copy that is part of the control chrome rather than displayed data

When the same control shows both chrome and data (for example an editable Notes field), the **data content** must still be selectable.

## Expectations

- Selection behavior should match platform norms: click-drag to highlight, copy via standard OS shortcuts or context menu where applicable.
- Do not rely on a separate “copy” affordance as the only way to extract displayed data; selection-and-copy must work for the data itself.
- Read-only displayed data must remain selectable even when the surrounding view is not editable.
