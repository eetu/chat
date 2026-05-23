import { Theme, useTheme } from "@emotion/react";
import {
  Link,
  useLocation,
  useNavigate,
  useParams,
} from "@tanstack/react-router";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import useSWR, { useSWRConfig } from "swr";

import { api, Conversation, Me } from "../api";
import { mq } from "../mq";
import SwipeRow from "./SwipeRow";
import Wordmark from "./Wordmark";

type Props = {
  /** Closes the sidebar (collapses on desktop, slides drawer out on mobile). */
  onClose?: () => void;
  /** Opens the global search palette. Threaded down from `__root` so
   * the keyboard shortcut and the sidebar button share one source of
   * truth for the open state. */
  onOpenSearch?: () => void;
};

const Sidebar = ({ onClose, onOpenSearch }: Props) => {
  const theme = useTheme();
  const navigate = useNavigate();
  const params = useParams({ strict: false }) as { id?: string };
  const activeId = params.id;

  const { data, mutate } = useSWR<Conversation[]>(
    "/api/conversations",
    api.listConversations,
  );
  const { data: me } = useSWR<Me>("/api/me", api.me);
  const location = useLocation();
  const onSettings = location.pathname === "/settings";

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
            <span className="material-symbols-outlined" css={{ fontSize: 20 }}>
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
        <span className="material-symbols-outlined" css={{ fontSize: 18 }}>
          add
        </span>
        new chat
      </button>

      {onOpenSearch && (
        <button
          type="button"
          onClick={onOpenSearch}
          aria-label="search conversations"
          css={{
            margin: "0 12px 8px",
            padding: "8px 12px",
            borderRadius: theme.border.radius,
            border: `1px solid ${theme.colors.border}`,
            background: "transparent",
            color: theme.colors.text.muted,
            fontFamily: theme.fonts.heading,
            fontSize: 13,
            textAlign: "left",
            cursor: "pointer",
            display: "flex",
            alignItems: "center",
            gap: 8,
            "&:hover": {
              color: theme.colors.text.main,
              background: theme.colors.background.main,
            },
          }}
        >
          <span className="material-symbols-outlined" css={{ fontSize: 16 }}>
            search
          </span>
          <span css={{ flex: 1 }}>search…</span>
          <kbd
            aria-hidden
            css={{
              ...theme.typography.caption,
              fontFamily:
                "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
              fontSize: 11,
              color: theme.colors.text.muted,
              border: `1px solid ${theme.colors.border}`,
              borderRadius: 3,
              padding: "0 5px",
            }}
          >
            ⌘K
          </kbd>
        </button>
      )}

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
          <ConversationRow
            key={c.id}
            convo={c}
            active={activeId === c.id}
            theme={theme}
            onDelete={() => onDelete(c.id)}
          />
        ))}
      </div>

      {me && (
        <button
          type="button"
          aria-label="open settings"
          aria-current={onSettings ? "page" : undefined}
          onClick={() => navigate({ to: "/settings" })}
          css={{
            width: "100%",
            padding: "10px 12px 12px 16px",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 8,
            border: "none",
            borderTop: `1px solid ${theme.colors.border}`,
            background: onSettings
              ? theme.colors.activity.onSoft
              : "transparent",
            color: theme.colors.text.main,
            cursor: "pointer",
            textAlign: "left",
            "&:hover": { background: theme.colors.background.main },
          }}
        >
          <span
            css={{
              ...theme.typography.body2,
              color: theme.colors.text.main,
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
              flex: 1,
            }}
            title={me.username}
          >
            {me.username}
          </span>
          <span
            className="material-symbols-outlined"
            aria-hidden
            css={{
              fontSize: 20,
              color: theme.colors.text.muted,
            }}
          >
            chevron_right
          </span>
        </button>
      )}
    </aside>
  );
};

const ConversationRow = ({
  convo,
  active,
  theme,
  onDelete,
}: {
  convo: Conversation;
  active: boolean;
  theme: Theme;
  onDelete: () => void;
}) => {
  const { mutate } = useSWRConfig();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(convo.title);
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!editing) return;
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    el.select();
  }, [editing]);

  const startEdit = () => {
    setDraft(convo.title);
    setEditing(true);
  };

  const cancel = () => {
    setEditing(false);
    setDraft(convo.title);
  };

  const commit = async () => {
    const next = draft.trim();
    if (!next || next === convo.title) {
      cancel();
      return;
    }
    setSaving(true);
    try {
      await api.renameConversation(convo.id, next);
      await mutate("/api/conversations");
      setEditing(false);
    } catch (e) {
      console.error("rename failed", e);
      cancel();
    } finally {
      setSaving(false);
    }
  };

  return (
    <SwipeRow onDelete={onDelete} hideMouseDelete={editing}>
      <div css={{ position: "relative" }}>
        {editing ? (
          <div
            css={{
              padding: "10px 16px",
              borderLeft: `2px solid ${theme.colors.activity.on}`,
              background: theme.colors.activity.onSoft,
            }}
          >
            <input
              ref={inputRef}
              value={draft}
              disabled={saving}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void commit();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  cancel();
                }
              }}
              onBlur={() => void commit()}
              maxLength={120}
              css={{
                ...theme.typography.body2,
                width: "100%",
                background: theme.colors.background.main,
                border: `1px solid ${theme.colors.activity.on}`,
                borderRadius: 4,
                padding: "3px 6px",
                color: theme.colors.text.main,
                outline: "none",
              }}
            />
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.text.muted,
                marginTop: 4,
              }}
            >
              enter to save · esc to cancel
            </div>
          </div>
        ) : (
          <Link
            to="/c/$id"
            params={{ id: convo.id }}
            onDoubleClick={(e) => {
              // Desktop convenience: dbl-click the title row to rename
              // inline. Mobile loses this affordance but swipe still
              // handles delete; rename via a longer hold collides with
              // iOS Safari's link-preview popup, so we don't bind it.
              e.preventDefault();
              e.stopPropagation();
              startEdit();
            }}
            css={{
              display: "block",
              padding: "10px 16px",
              borderLeft: `2px solid ${
                active ? theme.colors.activity.on : "transparent"
              }`,
              background: active ? theme.colors.activity.onSoft : "transparent",
              "@media (hover: hover)": {
                paddingRight: 40,
                "&:hover": {
                  background: active
                    ? theme.colors.activity.onSoft
                    : theme.colors.background.main,
                },
              },
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
              {convo.title || "untitled"}
            </div>
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.text.muted,
                marginTop: 2,
              }}
            >
              {relativeTime(convo.updated_at)}
            </div>
          </Link>
        )}
      </div>
    </SwipeRow>
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
