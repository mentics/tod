import {
  Button,
  Divider,
  H2,
  Row,
  Spacer,
  Stack,
  Text,
  TextArea,
  useCanvasState,
  useHostTheme,
} from "cursor/canvas";

type Role = "user" | "assistant";

type Turn = {
  id: string;
  role: Role;
  body: string;
  /** Excerpt the user can target with Use this */
  usable?: string;
};

const PARENT = {
  id: "q-031",
  short: "Visual fit",
};

const TURNS: Turn[] = [
  {
    id: "t1",
    role: "user",
    body: "We're stuck on how visual design should fit this interview task. Completeness currently waits on Accepted visual packages under artifacts/. Walk me through the trade-offs of HTML mockups vs a visual-design agent in process.",
  },
  {
    id: "t2",
    role: "assistant",
    body: "Both paths can work; they optimize different things.\n\nHTML mockups under artifacts/ keep co-design durable and reviewable next to the task — reviewers open a file, not a chat. A visual-design agent is faster for exploration but needs an explicit handoff into artifacts/ or it evaporates.\n\nFor this process, prefer HTML (or equivalent) packages under artifacts/visual/{screen}/, Accept them in design.md Links, and treat the agent as the authoring loop — not the source of truth.",
    usable:
      "Prefer HTML mockups under artifacts/visual/{screen}/; agent is the authoring loop, Accepted packages are the source of truth. Link Accepted packages from design.md.",
  },
  {
    id: "t3",
    role: "user",
    body: "What should I paste back into the parent answer if we choose option A?",
  },
  {
    id: "t4",
    role: "assistant",
    body: "Keep the parent answer short. Select the recommendation below, hit Use this to paste into the parent Notes field, edit if needed, then Submit on the interview workspace — deep dive never auto-submits.",
    usable:
      "A — HTML mockups under artifacts/. Completeness requires Accepted (or waived) visual packages; co-design via visual-design agent, durable output under task artifacts/.",
  },
];

export default function InterviewDeepDive() {
  const theme = useHostTheme();
  const [draft, setDraft] = useCanvasState("draft", "");
  const [targetId, setTargetId] = useCanvasState("targetId", "t4");
  const [pastedNote, setPastedNote] = useCanvasState("pastedNote", "");

  const target = TURNS.find((t) => t.id === targetId && t.usable);

  return (
    <Stack gap={16} style={{ padding: 16, minHeight: "100%" }}>
      <Stack gap={4}>
        <H2>Interview — deep dive</H2>
        <Text tone="secondary" size="small">
          Separate agent chat · Use this pastes into parent answer · no auto-submit
        </Text>
      </Stack>

      <Stack
        gap={0}
        style={{
          border: `1px solid ${theme.stroke.secondary}`,
          background: theme.bg.elevated,
          alignSelf: "flex-start",
          width: "100%",
          maxWidth: 720,
          minHeight: 520,
        }}
      >
        {/* Quiet parent context */}
        <Row
          gap={8}
          align="center"
          style={{
            padding: "8px 14px",
            borderBottom: `1px solid ${theme.stroke.tertiary}`,
            background: theme.fill.tertiary,
          }}
        >
          <Text size="small" tone="tertiary">
            Parent
          </Text>
          <Text size="small" tone="secondary">
            {PARENT.id}
          </Text>
          <Text size="small" weight="semibold">
            {PARENT.short}
          </Text>
          <Spacer />
          <Text size="small" tone="tertiary">
            Parent inputs unchanged
          </Text>
        </Row>

        {/* Transcript */}
        <Stack
          gap={12}
          style={{
            flex: 1,
            padding: "14px 14px 8px",
            minHeight: 340,
          }}
        >
          {TURNS.map((turn) => {
            const isAssistant = turn.role === "assistant";
            const isTarget = targetId === turn.id && !!turn.usable;
            return (
              <div key={turn.id}>
                <Stack
                  gap={6}
                  style={{
                    padding: "8px 10px",
                    background: isTarget
                      ? theme.fill.secondary
                      : "transparent",
                    borderLeft: isTarget
                      ? `2px solid ${theme.accent.primary}`
                      : "2px solid transparent",
                  }}
                >
                  <Text
                    size="small"
                    weight="semibold"
                    style={{
                      color: isAssistant
                        ? theme.accent.primary
                        : theme.text.tertiary,
                    }}
                  >
                    {isAssistant ? "Agent" : "You"}
                  </Text>
                  <Text
                    size="small"
                    style={{ lineHeight: 1.45, whiteSpace: "pre-wrap" }}
                  >
                    {turn.body}
                  </Text>
                  {turn.usable ? (
                    <Stack
                      gap={6}
                      style={{
                        marginTop: 4,
                        padding: "8px 10px",
                        border: `1px solid ${theme.stroke.tertiary}`,
                        background: theme.bg.chrome,
                      }}
                    >
                      <Text size="small" tone="tertiary">
                        Target for parent answer
                      </Text>
                      <Text size="small" style={{ lineHeight: 1.4 }}>
                        {turn.usable}
                      </Text>
                      <Row gap={8} align="center">
                        <Button
                          variant={isTarget ? "primary" : "secondary"}
                          onClick={() => {
                            setTargetId(turn.id);
                            setPastedNote(turn.usable!);
                          }}
                        >
                          Use this
                        </Button>
                        {isTarget && pastedNote === turn.usable ? (
                          <Text size="small" tone="tertiary">
                            Pasted into parent answer area
                          </Text>
                        ) : null}
                      </Row>
                    </Stack>
                  ) : null}
                </Stack>
              </div>
            );
          })}
        </Stack>

        <Divider />

        {/* Chat input */}
        <Stack gap={8} style={{ padding: "10px 14px 14px" }}>
          {pastedNote ? (
            <Text size="small" tone="tertiary">
              Parent answer preview: {pastedNote.slice(0, 72)}
              {pastedNote.length > 72 ? "…" : ""}
            </Text>
          ) : (
            <Text size="small" tone="tertiary">
              Select a target below an agent turn, then Use this — edits and
              submit stay on the parent question.
            </Text>
          )}
          <Row gap={8} align="end">
            <div style={{ flex: 1, minWidth: 0 }}>
              <TextArea
                value={draft}
                onChange={setDraft}
                placeholder="Message deep-dive agent…"
                rows={2}
              />
            </div>
            <Button variant="primary" onClick={() => setDraft("")}>
              Send
            </Button>
          </Row>
          {target ? (
            <Text size="small" tone="tertiary">
              Active target · {target.id} · pastes to {PARENT.id} Notes
            </Text>
          ) : null}
        </Stack>
      </Stack>
    </Stack>
  );
}
