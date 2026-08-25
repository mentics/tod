import {
  Button,
  Divider,
  Row,
  Spacer,
  Stack,
  Text,
  TextInput,
  useCanvasState,
  useHostTheme,
} from "cursor/canvas";

type SettingRowProps = {
  value: string;
  onChange: (v: string) => void;
  label: string;
  help: string;
};

function SettingRow({ value, onChange, label, help }: SettingRowProps) {
  const theme = useHostTheme();
  return (
    <Row gap={12} align="start">
      <TextInput
        type="number"
        value={value}
        onChange={onChange}
        style={{ width: 56, flexShrink: 0 }}
      />
      <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
        <Text size="small" weight="semibold">
          {label}
        </Text>
        <Text size="small" tone="tertiary" style={{ lineHeight: 1.35 }}>
          {help}
        </Text>
      </Stack>
    </Row>
  );
}

/** Compact Settings — inputs left, label + help right; no persistence path chrome. */
export default function InterviewSettings() {
  const theme = useHostTheme();
  const [replenishBelow, setReplenishBelow] = useCanvasState(
    "replenishBelow",
    "8",
  );
  const [secondBelow, setSecondBelow] = useCanvasState("secondBelow", "2");

  return (
    <Stack
      gap={0}
      style={{
        padding: 12,
        minHeight: "100%",
        background: theme.bg.editor,
      }}
    >
      <Row
        gap={8}
        align="center"
        style={{
          paddingBottom: 8,
          borderBottom: `1px solid ${theme.stroke.tertiary}`,
          marginBottom: 10,
        }}
      >
        <Text weight="semibold" size="small">
          tod
        </Text>
        <Text size="small" tone="tertiary">
          · Tasks / Interview ·
        </Text>
        <Text
          size="small"
          weight="semibold"
          style={{ color: theme.accent.primary }}
        >
          Settings
        </Text>
      </Row>

      <Stack
        gap={12}
        style={{
          border: `1px solid ${theme.stroke.secondary}`,
          background: theme.bg.elevated,
          padding: "10px 12px",
          maxWidth: 440,
          alignSelf: "flex-start",
          width: "100%",
        }}
      >
        <Text weight="semibold" size="small">
          Researcher thresholds
        </Text>

        <SettingRow
          value={replenishBelow}
          onChange={setReplenishBelow}
          label="Replenish below"
          help="Start a researcher run when open questions fall under this count. Default 8."
        />

        <SettingRow
          value={secondBelow}
          onChange={setSecondBelow}
          label="Second researcher below"
          help="While one researcher is already running, start a second if open count drops under this lower threshold. Max two runs. Default 2."
        />

        <Divider />

        <Row gap={8} align="center">
          <Button variant="ghost" onClick={() => undefined}>
            Cancel
          </Button>
          <Spacer />
          <Button variant="primary" onClick={() => undefined}>
            Save
          </Button>
        </Row>
      </Stack>
    </Stack>
  );
}
