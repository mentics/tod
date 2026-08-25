import {
  Button,
  Divider,
  H2,
  Row,
  Select,
  Spacer,
  Stack,
  Text,
  TextInput,
  useCanvasState,
  useHostTheme,
} from "cursor/canvas";

type SessionStatus = "active" | "complete" | "archived";
type FilterTab = "active" | "archive";

type Session = {
  id: string;
  name: string;
  entity: string;
  status: SessionStatus;
  updated: string;
};

const SESSIONS: Session[] = [
  {
    id: "s-014",
    name: "Core UI — design interview",
    entity: "interview-ui / core-ui",
    status: "active",
    updated: "Updated 2h ago",
  },
  {
    id: "s-013",
    name: "ACP billing spike follow-up",
    entity: "interview-ui / spikes",
    status: "active",
    updated: "Updated yesterday",
  },
  {
    id: "s-012",
    name: "UI scaffolding — requirements",
    entity: "tod / ui-scaffolding",
    status: "complete",
    updated: "Updated 3d ago",
  },
  {
    id: "s-011",
    name: "Project defining interview",
    entity: "interview-ui",
    status: "archived",
    updated: "Archived Aug 20",
  },
  {
    id: "s-010",
    name: "Process bootstrap interview",
    entity: "tod / bootstrap",
    status: "archived",
    updated: "Archived Aug 18",
  },
  {
    id: "s-009",
    name: "Researcher threshold defaults",
    entity: "interview-ui / settings",
    status: "archived",
    updated: "Archived Aug 15",
  },
];

const ENTITIES = [
  { value: "interview-ui/core-ui", label: "interview-ui / core-ui" },
  { value: "interview-ui", label: "interview-ui (project)" },
  { value: "tod/ui-scaffolding", label: "tod / ui-scaffolding" },
  { value: "new-project", label: "New project…" },
  { value: "new-task", label: "New task…" },
];

const PURPOSES = [
  { value: "design", label: "Design phase" },
  { value: "planning", label: "Planning phase" },
  { value: "initial", label: "Initial / defining" },
  { value: "other", label: "Other purpose" },
];

function statusLabel(status: SessionStatus): string {
  if (status === "active") return "Active";
  if (status === "complete") return "Complete";
  return "Archived";
}

export default function InterviewSessionsMenu() {
  const theme = useHostTheme();
  const [navTab, setNavTab] = useCanvasState<"tasks" | "interview">(
    "navTab",
    "interview",
  );
  const [filter, setFilter] = useCanvasState<FilterTab>("filter", "active");
  const [selectedId, setSelectedId] = useCanvasState("selectedId", "s-014");
  const [composing, setComposing] = useCanvasState("composing", false);
  const [entity, setEntity] = useCanvasState("entity", "interview-ui/core-ui");
  const [purpose, setPurpose] = useCanvasState("purpose", "design");
  const [purposeNote, setPurposeNote] = useCanvasState("purposeNote", "");

  const visible = SESSIONS.filter((s) =>
    filter === "archive" ? s.status === "archived" : s.status !== "archived",
  );
  const selected = visible.find((s) => s.id === selectedId) ?? visible[0];
  const archivedSelected = selected?.status === "archived";

  return (
    <Stack gap={0} style={{ minHeight: "100%", background: theme.bg.editor }}>
      {/* App chrome — tod · Tasks / Interview · Settings */}
      <Row
        align="center"
        gap={12}
        style={{
          padding: "8px 14px",
          borderBottom: `1px solid ${theme.stroke.tertiary}`,
          background: theme.bg.chrome,
        }}
      >
        <Text weight="semibold" size="small">
          tod
        </Text>
        <Text tone="tertiary" size="small">
          ·
        </Text>
        <Row gap={4} align="center">
          {(
            [
              { id: "tasks" as const, label: "Tasks" },
              { id: "interview" as const, label: "Interview" },
            ] as const
          ).map((tab) => {
            const on = navTab === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => setNavTab(tab.id)}
                style={{
                  border: "none",
                  borderBottom: on
                    ? `2px solid ${theme.accent.primary}`
                    : "2px solid transparent",
                  background: "transparent",
                  color: on ? theme.text.primary : theme.text.tertiary,
                  padding: "4px 8px",
                  cursor: "pointer",
                  font: "inherit",
                  fontWeight: on ? 600 : 400,
                  fontSize: 13,
                }}
              >
                {tab.label}
              </button>
            );
          })}
        </Row>
        <Spacer />
        <Button variant="ghost" onClick={() => undefined}>
          Settings
        </Button>
      </Row>

      <Stack
        gap={12}
        style={{
          padding: 16,
          maxWidth: 720,
          width: "100%",
          alignSelf: "center",
          boxSizing: "border-box",
        }}
      >
        <Row align="center" gap={12}>
          <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
            <H2>Interviews</H2>
            <Text tone="secondary" size="small">
              Session list is the menu — start new interviews here; no separate
              launch screen.
            </Text>
          </Stack>
          <Button
            variant="primary"
            onClick={() => setComposing((v) => !v)}
          >
            {composing ? "Cancel" : "New interview"}
          </Button>
        </Row>

        {/* Lightweight in-menu new-interview controls */}
        {composing ? (
          <Stack
            gap={10}
            style={{
              padding: 12,
              border: `1px solid ${theme.stroke.secondary}`,
              background: theme.bg.elevated,
            }}
          >
            <Text weight="semibold" size="small">
              Start interview
            </Text>
            <Row gap={10} wrap align="end">
              <Stack gap={4} style={{ flex: 1, minWidth: 180 }}>
                <Text size="small" tone="tertiary">
                  Entity
                </Text>
                <Select
                  value={entity}
                  onChange={setEntity}
                  options={ENTITIES}
                  style={{ width: "100%", boxSizing: "border-box" }}
                />
              </Stack>
              <Stack gap={4} style={{ flex: 1, minWidth: 160 }}>
                <Text size="small" tone="tertiary">
                  Purpose
                </Text>
                <Select
                  value={purpose}
                  onChange={setPurpose}
                  options={PURPOSES}
                  style={{ width: "100%", boxSizing: "border-box" }}
                />
              </Stack>
            </Row>
            <Stack gap={4}>
              <Text size="small" tone="tertiary">
                Context (optional)
              </Text>
              <TextInput
                value={purposeNote}
                onChange={setPurposeNote}
                placeholder="What should the researcher focus on?"
              />
            </Stack>
            <Row gap={8} justify="end">
              <Button variant="ghost" onClick={() => setComposing(false)}>
                Cancel
              </Button>
              <Button variant="primary" onClick={() => setComposing(false)}>
                Start
              </Button>
            </Row>
          </Stack>
        ) : null}

        {/* Active / Archive filter */}
        <Row
          gap={0}
          align="center"
          style={{
            borderBottom: `1px solid ${theme.stroke.tertiary}`,
          }}
        >
          {(
            [
              { id: "active" as const, label: "Active" },
              { id: "archive" as const, label: "Archive" },
            ] as const
          ).map((tab) => {
            const on = filter === tab.id;
            const count =
              tab.id === "archive"
                ? SESSIONS.filter((s) => s.status === "archived").length
                : SESSIONS.filter((s) => s.status !== "archived").length;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => {
                  setFilter(tab.id);
                  const next = SESSIONS.find((s) =>
                    tab.id === "archive"
                      ? s.status === "archived"
                      : s.status !== "archived",
                  );
                  if (next) setSelectedId(next.id);
                }}
                style={{
                  border: "none",
                  borderBottom: on
                    ? `2px solid ${theme.accent.primary}`
                    : "2px solid transparent",
                  background: "transparent",
                  color: on ? theme.text.primary : theme.text.tertiary,
                  padding: "6px 12px 8px",
                  cursor: "pointer",
                  font: "inherit",
                  fontWeight: on ? 600 : 400,
                  fontSize: 13,
                  marginBottom: -1,
                }}
              >
                {tab.label}
                <Text
                  as="span"
                  size="small"
                  tone="tertiary"
                  style={{ marginLeft: 6 }}
                >
                  {count}
                </Text>
              </button>
            );
          })}
        </Row>

        {/* Session list */}
        <Stack
          gap={0}
          style={{
            border: `1px solid ${theme.stroke.secondary}`,
            background: theme.bg.elevated,
          }}
        >
          {visible.map((s, i) => {
            const isSelected = selected?.id === s.id;
            return (
              <button
                key={s.id}
                type="button"
                onClick={() => setSelectedId(s.id)}
                style={{
                  display: "block",
                  width: "100%",
                  textAlign: "left",
                  border: "none",
                  borderTop:
                    i === 0 ? "none" : `1px solid ${theme.stroke.tertiary}`,
                  borderLeft: isSelected
                    ? `2px solid ${theme.accent.primary}`
                    : "2px solid transparent",
                  background: isSelected
                    ? theme.fill.secondary
                    : "transparent",
                  color: theme.text.primary,
                  padding: "10px 12px 10px 10px",
                  cursor: "pointer",
                  font: "inherit",
                }}
              >
                <Row align="center" gap={10}>
                  <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
                    <Text
                      weight={isSelected ? "semibold" : "normal"}
                      style={{
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {s.name}
                    </Text>
                    <Text size="small" tone="tertiary">
                      {s.entity}
                      {"  ·  "}
                      {statusLabel(s.status)}
                      {"  ·  "}
                      {s.updated}
                    </Text>
                  </Stack>
                  {isSelected ? (
                    <Button
                      variant={archivedSelected ? "secondary" : "primary"}
                      onClick={() => undefined}
                    >
                      Open
                    </Button>
                  ) : null}
                </Row>
              </button>
            );
          })}
        </Stack>

        {filter === "archive" ? (
          <Text size="small" tone="tertiary">
            Archived sessions reopen read-only for agent work — answer submit
            and replenishment stay blocked.
          </Text>
        ) : (
          <Text size="small" tone="tertiary">
            Active includes in-progress and complete sessions. Archive from an
            open session when finished.
          </Text>
        )}

        <Divider />

        <Text size="small" tone="tertiary">
          Mockup · session list / menu · matches interview workspace chrome
          family
        </Text>
      </Stack>
    </Stack>
  );
}
