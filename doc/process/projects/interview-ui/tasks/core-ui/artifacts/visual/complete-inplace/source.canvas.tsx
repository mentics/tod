import {
  Button,
  H2,
  Link,
  Row,
  Stack,
  Text,
  useHostTheme,
} from "cursor/canvas";

/**
 * Interview workspace — Complete (in-place).
 * Same three-column shell as the question layout; queue empty and
 * replenishment not waiting. Not a dedicated finished screen or modal.
 */
export default function InterviewCompleteInPlace() {
  const theme = useHostTheme();

  return (
    <Stack gap={16} style={{ padding: 16, minHeight: "100%" }}>
      <Stack gap={4}>
        <H2>Interview — question workspace</H2>
        <Text tone="secondary" size="small">
          Same shell · queue empty · replenishment idle (in-place Complete)
        </Text>
      </Stack>

      <Row
        gap={0}
        align="stretch"
        style={{
          border: `1px solid ${theme.stroke.secondary}`,
          background: theme.bg.elevated,
          alignSelf: "flex-start",
          width: "100%",
          maxWidth: 920,
          minHeight: 280,
        }}
      >
        {/* Col 1 — empty question list */}
        <Stack
          gap={0}
          style={{
            width: 196,
            flexShrink: 0,
            borderRight: `1px solid ${theme.stroke.tertiary}`,
            padding: "6px 0",
            minHeight: 280,
          }}
        />

        {/* Col 2 — Complete message (main) */}
        <Stack
          gap={12}
          style={{
            flex: 1.15,
            minWidth: 200,
            padding: "28px 20px",
            borderRight: `1px solid ${theme.stroke.tertiary}`,
            justifyContent: "center",
          }}
        >
          <Stack gap={6}>
            <Text weight="semibold" style={{ fontSize: 15 }}>
              Complete
            </Text>
            <Text tone="secondary" style={{ lineHeight: 1.45, maxWidth: 360 }}>
              No open questions remain, and the researcher added none. This
              interview is finished.
            </Text>
          </Stack>

          <Row gap={12} align="center" wrap>
            <Button variant="primary" onClick={() => undefined}>
              Back to interviews
            </Button>
            <Link
              href="#related-docs"
              style={{ fontSize: 12, color: theme.text.tertiary }}
            >
              Open related docs
            </Link>
          </Row>
        </Stack>

        {/* Col 3 — response pane idle (no controls) */}
        <Stack
          gap={0}
          style={{
            flex: 0.85,
            minWidth: 200,
            maxWidth: 280,
            padding: "12px 14px",
            background: theme.fill.tertiary,
          }}
        />
      </Row>
    </Stack>
  );
}
