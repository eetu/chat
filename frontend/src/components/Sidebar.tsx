import { Theme, useTheme } from "@emotion/react";
import {
  Link,
  useLocation,
  useNavigate,
  useParams,
} from "@tanstack/react-router";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import useSWR, { useSWRConfig } from "swr";

import { api, Conversation, Me } from "../api";
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
            className="material-icons-outlined"
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
  const [menuOpen, setMenuOpen] = useState(false);
  const [menuPos, setMenuPos] = useState<{ top: number; right: number } | null>(
    null,
  );
  const inputRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!editing) return;
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    el.select();
  }, [editing]);

  useEffect(() => {
    if (!menuOpen) return;
    const onPointer = (e: PointerEvent) => {
      const el = menuRef.current;
      const trigger = triggerRef.current;
      if (!(e.target instanceof Node)) return;
      // Clicks on the trigger itself are handled by its own onClick; the
      // outside-pointer guard only closes for things outside both the
      // menu and the trigger button.
      if (el?.contains(e.target) || trigger?.contains(e.target)) return;
      setMenuOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    const onScroll = () => setMenuOpen(false);
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey);
    // Capture-phase scroll so it fires for any scroll container parent
    // of the sidebar (the conversation list itself, mainly). Position is
    // anchored to viewport coords; scrolling invalidates it.
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [menuOpen]);

  const openMenu = () => {
    const rect = triggerRef.current?.getBoundingClientRect();
    if (rect) {
      setMenuPos({
        top: rect.bottom + 4,
        right: Math.max(8, window.innerWidth - rect.right),
      });
    }
    setMenuOpen(true);
  };

  const startEdit = () => {
    setMenuOpen(false);
    setDraft(convo.title);
    setEditing(true);
  };

  const requestDelete = () => {
    setMenuOpen(false);
    if (window.confirm("delete this conversation?")) onDelete();
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
    <SwipeRow onDelete={onDelete} hideMouseDelete>
      <div
        css={{
          position: "relative",
          "& .row-menu-trigger": {
            opacity: 0,
            transition: "opacity 120ms ease",
          },
          "&:hover .row-menu-trigger, &:focus-within .row-menu-trigger": {
            opacity: 1,
          },
          "@media (hover: none)": {
            "& .row-menu-trigger": { opacity: 1 },
          },
        }}
      >
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
            css={{
              display: "block",
              padding: "10px 16px",
              paddingRight: 40,
              borderLeft: `2px solid ${
                active ? theme.colors.activity.on : "transparent"
              }`,
              background: active ? theme.colors.activity.onSoft : "transparent",
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
        {!editing && (
          <button
            ref={triggerRef}
            type="button"
            className="row-menu-trigger"
            aria-label="conversation actions"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            title="actions"
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              if (menuOpen) setMenuOpen(false);
              else openMenu();
            }}
            css={{
              position: "absolute",
              top: 8,
              right: 8,
              width: 26,
              height: 26,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              border: "none",
              borderRadius: 4,
              background: menuOpen
                ? theme.colors.background.main
                : "transparent",
              color: menuOpen
                ? theme.colors.text.main
                : theme.colors.text.muted,
              cursor: "pointer",
              "&:hover": {
                background: theme.colors.background.main,
                color: theme.colors.text.main,
              },
            }}
          >
            <span className="material-icons-outlined" css={{ fontSize: 18 }}>
              more_vert
            </span>
          </button>
        )}
        {/* Portal the menu to body so SwipeRow's overflow:hidden doesn't
            clip it against the next row. Position is captured from the
            trigger's bounding rect at open-time; window scroll / resize
            close the menu instead of chasing the trigger live. */}
        {menuOpen &&
          menuPos &&
          createPortal(
            <div
              ref={menuRef}
              role="menu"
              css={{
                position: "fixed",
                top: menuPos.top,
                right: menuPos.right,
                zIndex: 50,
                minWidth: 160,
                background: theme.colors.background.main,
                border: `1px solid ${theme.colors.border}`,
                borderRadius: theme.border.radius,
                boxShadow: theme.shadows.main,
                padding: 4,
                display: "flex",
                flexDirection: "column",
              }}
            >
              <MenuItem
                theme={theme}
                icon="edit"
                label="rename"
                onSelect={startEdit}
              />
              <MenuItem
                theme={theme}
                icon="delete_outline"
                label="delete"
                danger
                onSelect={requestDelete}
              />
            </div>,
            document.body,
          )}
      </div>
    </SwipeRow>
  );
};

const MenuItem = ({
  theme,
  icon,
  label,
  danger,
  onSelect,
}: {
  theme: Theme;
  icon: string;
  label: string;
  danger?: boolean;
  onSelect: () => void;
}) => (
  <button
    type="button"
    role="menuitem"
    onClick={(e) => {
      e.preventDefault();
      e.stopPropagation();
      onSelect();
    }}
    css={{
      ...theme.typography.body2,
      display: "flex",
      alignItems: "center",
      gap: 8,
      padding: "8px 10px",
      border: "none",
      background: "transparent",
      color: danger ? theme.colors.error : theme.colors.text.main,
      textAlign: "left",
      cursor: "pointer",
      borderRadius: 4,
      "&:hover": {
        background: danger ? theme.colors.error : theme.colors.background.light,
        color: danger ? "#fff" : theme.colors.text.main,
      },
    }}
  >
    <span className="material-icons-outlined" css={{ fontSize: 18 }}>
      {icon}
    </span>
    {label}
  </button>
);

function relativeTime(unix: number): string {
  const diff = Date.now() / 1000 - unix;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

export default memo(Sidebar);
