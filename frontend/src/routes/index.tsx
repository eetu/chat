/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useMemo, useState } from "react";
import useSWR, { useSWRConfig } from "swr";

import { api, ModelCapabilities } from "../api";
import Composer from "../components/Composer";
import Wordmark from "../components/Wordmark";
import { mq } from "../mq";

const LAST_MODEL_KEY = "chat:lastModel";

const Landing = () => {
  const theme = useTheme();
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();

  const [model, setModel] = useState<string | null>(() => {
    try {
      return window.localStorage.getItem(LAST_MODEL_KEY);
    } catch {
      return null;
    }
  });

  const { data: modelsData } = useSWR("/api/models", api.models);
  const availableModels = useMemo(
    () =>
      (modelsData?.models ?? [])
        .map((m) => m.name)
        .filter((n): n is string => !!n),
    [modelsData],
  );
  if (
    availableModels.length > 0 &&
    (!model || !availableModels.includes(model))
  ) {
    setModel(availableModels[0]);
  }

  const { data: caps } = useSWR<ModelCapabilities>(
    model ? ["caps", model] : null,
    () => api.modelCaps(model as string),
  );

  const onModelChange = (next: string) => {
    setModel(next);
    try {
      window.localStorage.setItem(LAST_MODEL_KEY, next);
    } catch {
      // ignore storage errors (private mode, quota)
    }
  };

  const onSend = async (content: string, images?: string[]) => {
    const conv = await api.createConversation({ model: model ?? undefined });
    try {
      window.sessionStorage.setItem(
        `chat:pending:${conv.id}`,
        JSON.stringify({ content, images, model }),
      );
    } catch {
      // ignore
    }
    await mutate("/api/conversations");
    navigate({ to: "/c/$id", params: { id: conv.id } });
  };

  return (
    <div
      css={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 24,
        padding: "60px 24px 24px",
        [mq[0]]: { padding: "60px 16px 16px" },
      }}
    >
      <Wordmark size={32} />
      <p
        css={{
          ...theme.typography.body2,
          color: theme.colors.text.muted,
          maxWidth: 360,
          textAlign: "center",
        }}
      >
        the path of the righteous prompt is beset on all sides.
      </p>
      <div css={{ width: "100%", maxWidth: 790 }}>
        <Composer
          onSend={onSend}
          model={model}
          onModelChange={onModelChange}
          vision={caps?.vision ?? false}
        />
      </div>
    </div>
  );
};

export const Route = createFileRoute("/")({
  component: Landing,
});
