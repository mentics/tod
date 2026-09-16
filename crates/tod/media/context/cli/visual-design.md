## `tod-cli visual-design`

The UI mockup associated with one obligation.

```
tod-cli --data-root <DATA_ROOT> visual-design save  --obligation <UUID> --html-file <PATH>
tod-cli --data-root <DATA_ROOT> visual-design show  --obligation <UUID>
tod-cli --data-root <DATA_ROOT> visual-design clear --obligation <UUID>
```

`save` writes the HTML file under the data root and links it from the given
obligation, replacing any mockup already linked there. The HTML must be
self-contained: no `<script>` tags, no external network resources.

Never write mockup files into the data root yourself, and never invent a
storage path — always go through `save`, the same way obligations are only ever
written through `tod-cli obligations`.
