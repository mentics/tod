## `tod-cli visual-design`

```
tod-cli --data-root <DATA_ROOT> visual-design save --obligation <OBLIGATION_UUID> --html-file <PATH>
```

Writes the mockup file under the data root and links it from that obligation,
replacing any mockup already linked there.

Never write mockup files into the data root yourself, and never invent a
storage path — always go through this command, the same way obligations are
only ever written through `tod-cli obligations`.
