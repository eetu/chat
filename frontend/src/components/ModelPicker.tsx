import { useTheme } from "@emotion/react";
import { memo } from "react";
import useSWR from "swr";

import { api } from "../api";

type ModelEntry = {
  name?: string;
  model?: string;
  locked?: boolean;
};

type Props = {
  value: string | null;
  onChange: (next: string) => void;
  /** When true, the picker renders dimmed and non-interactive. Used in
   * img2img mode where the selected chat model has no bearing on the
   * upstream that runs the job. */
  disabled?: boolean;
};

/**
 * Native `<select>` model picker. When the backend returns a
 * `{ name, locked: true }` entry, the picker collapses to a read-only
 * label (server enforces `OLLAMA_MODEL`).
 */
const ModelPicker = ({ value, onChange, disabled }: Props) => {
  const theme = useTheme();
  const { data, error } = useSWR("/api/models", api.models);

  const models = (data?.models ?? []) as ModelEntry[];
  const names = models
    .map((m) => m.name ?? m.model ?? "")
    .filter((n) => n.length > 0);
  const locked = models.some((m) => m.locked === true);

  if (error) {
    return (
      <span
        css={{
          ...theme.typography.caption,
          color: theme.colors.error,
        }}
        title={String(error)}
      >
        models offline
      </span>
    );
  }

  if (locked) {
    return (
      <span
        css={{
          display: "inline-flex",
          alignItems: "center",
          gap: 4,
          ...theme.typography.caption,
          color: theme.colors.text.muted,
        }}
        title="model locked by server config (OLLAMA_MODEL)"
      >
        <span className="material-symbols-outlined" css={{ fontSize: 14 }}>
          lock
        </span>
        {names[0] ?? "—"}
      </span>
    );
  }

  if (names.length === 0) {
    return (
      <span
        css={{ ...theme.typography.caption, color: theme.colors.text.muted }}
      >
        no models
      </span>
    );
  }

  // Show the current value even if it isn't in the list yet (e.g. server
  // memory still has it but it's been pruned from /api/tags).
  const options = value && !names.includes(value) ? [value, ...names] : names;

  return (
    <select
      value={value ?? names[0]}
      onChange={(e) => onChange(e.target.value)}
      disabled={disabled}
      title={
        disabled
          ? "model selection has no effect in img2img mode — flux kontext runs the job"
          : undefined
      }
      css={{
        ...theme.typography.body2,
        background: "transparent",
        border: "none",
        padding: "4px 4px",
        color: theme.colors.text.muted,
        cursor: disabled ? "not-allowed" : "pointer",
        maxWidth: 220,
        outline: "none",
        appearance: "auto",
        opacity: disabled ? 0.4 : 1,
        "&:hover": {
          color: disabled ? theme.colors.text.muted : theme.colors.text.main,
        },
        "&:focus": { color: theme.colors.text.main },
      }}
    >
      {options.map((m) => (
        <option key={m} value={m}>
          {m}
        </option>
      ))}
    </select>
  );
};

export default memo(ModelPicker);
