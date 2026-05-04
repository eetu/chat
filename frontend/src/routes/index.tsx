/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useSWRConfig } from "swr";

import { api } from "../api";
import Wordmark from "../components/Wordmark";
import { mq } from "../mq";

const Landing = () => {
  const theme = useTheme();
  const navigate = useNavigate();
  const { mutate } = useSWRConfig();

  const start = async () => {
    const conv = await api.createConversation();
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
        gap: 18,
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
      <button
        type="button"
        onClick={start}
        css={{
          padding: "10px 18px",
          borderRadius: theme.border.radius,
          border: `1px solid ${theme.colors.border}`,
          background: theme.colors.background.main,
          fontFamily: theme.fonts.heading,
          fontSize: 14,
          cursor: "pointer",
          color: theme.colors.text.main,
        }}
      >
        new chat
      </button>
    </div>
  );
};

export const Route = createFileRoute("/")({
  component: Landing,
});
