import { useTheme } from "@emotion/react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { memo, useCallback } from "react";
import useSWR from "swr";

import { api, Conversation } from "../api";
import { mq } from "../mq";
import SwipeRow from "./SwipeRow";
import Wordmark from "./Wordmark";

type Props = {
  /** Closes the sidebar (collapses on desktop, slides drawer out on mobile). */
  onClose?: () => void;
};

const Sidebar = ({ onClose }: Props) => {
  const theme = useTheme();
  const navigate = useNavigate();
  const params = useParams({ strict: false }) as { id?: string };
  const activeId = params.id;

  const { data, mutate } = useSWR<Conversation[]>(
    "/api/conversations",
    api.listConversations,
  );

  const onNewChat = useCallback(async () => {
    const conv = await api.createConversation();
    await mutate();
    navigate({ to: "/c/$id", params: { id: conv.id } });
  }, [mutate, navigate]);

  const onDelete = useCallback(
    async (id: string) => {
      await api.deleteConversation(id);
      await mutate();
      if (activeId === id) navigate({ to: "/" });
    },
    [activeId, mutate, navigate],
  );

  return (
    <aside
      css={{
        width: 280,
        flexShrink: 0,
        borderRight: `1px solid ${theme.colors.border}`,
        backgroundColor: theme.colors.background.light,
        display: "flex",
        flexDirection: "column",
        height: "100%",
        [mq[0]]: { width: "82vw", maxWidth: 320 },
      }}
    >
      <div
        css={{
          padding: "16px 12px 8px 16px",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <Wordmark size={20} />
        {onClose && (
          <button
            type="button"
            aria-label="hide sidebar"
            onClick={onClose}
            css={{
              width: 32,
              height: 32,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              border: "none",
              borderRadius: theme.border.radius,
              background: "transparent",
              color: theme.colors.text.muted,
              cursor: "pointer",
              "&:hover": {
                background: theme.colors.background.main,
                color: theme.colors.text.main,
              },
            }}
          >
            <span className="material-icons-outlined" css={{ fontSize: 20 }}>
              chevron_left
            </span>
          </button>
        )}
      </div>

      <button
        type="button"
        onClick={onNewChat}
        css={{
          margin: "8px 12px",
          padding: "10px 12px",
          borderRadius: theme.border.radius,
          border: `1px solid ${theme.colors.border}`,
          background: theme.colors.background.main,
          color: theme.colors.text.main,
          fontFamily: theme.fonts.heading,
          fontSize: 14,
          textAlign: "left",
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          gap: 8,
        }}
      >
        <span className="material-icons-outlined" css={{ fontSize: 18 }}>
          add
        </span>
        new chat
      </button>

      <div css={{ flex: 1, overflowY: "auto", padding: "4px 0 16px" }}>
        {data?.length === 0 && (
          <div
            css={{
              ...theme.typography.caption,
              color: theme.colors.text.muted,
              padding: "12px 16px",
            }}
          >
            no chats yet
          </div>
        )}
        {data?.map((c) => (
          <SwipeRow key={c.id} onDelete={() => onDelete(c.id)}>
            <Link
              to="/c/$id"
              params={{ id: c.id }}
              css={{
                display: "block",
                padding: "10px 16px",
                borderLeft: `2px solid ${
                  activeId === c.id ? theme.colors.activity.on : "transparent"
                }`,
                background:
                  activeId === c.id
                    ? theme.colors.activity.onSoft
                    : "transparent",
              }}
            >
              <div
                css={{
                  ...theme.typography.body2,
                  color: theme.colors.text.main,
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {c.title || "untitled"}
              </div>
              <div
                css={{
                  ...theme.typography.caption,
                  color: theme.colors.text.muted,
                  marginTop: 2,
                }}
              >
                {relativeTime(c.updated_at)}
              </div>
            </Link>
          </SwipeRow>
        ))}
      </div>
    </aside>
  );
};

function relativeTime(unix: number): string {
  const diff = Date.now() / 1000 - unix;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

export default memo(Sidebar);
