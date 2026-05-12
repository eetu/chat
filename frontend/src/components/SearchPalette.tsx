import { useTheme } from "@emotion/react";
import { useNavigate } from "@tanstack/react-router";
import { Fragment, KeyboardEvent, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { api, SearchHit } from "../api";
import { mq } from "../mq";
import { stripMarkdown } from "../tts";

type Props = {
  onClose: () => void;
};

const DEBOUNCE_MS = 180;

/**
 * Floating search palette. Renders into document.body so it overlays
 * the whole app regardless of sidebar / route state. Live-queries
 * `/api/search` with a small debounce; ↑ / ↓ move selection, Enter
 * navigates to the conversation, Esc closes.
 */
const SearchPalette = ({ onClose }: Props) => {
  const theme = useTheme();
  const navigate = useNavigate();
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [active, setActive] = useState(0);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      // Intentional reset — emptying the query clears state immediately.
      // eslint-disable-next-line @eslint-react/set-state-in-effect
      setHits([]);
      // eslint-disable-next-line @eslint-react/set-state-in-effect
      setLoading(false);
      // eslint-disable-next-line @eslint-react/set-state-in-effect
      setError(null);
      return;
    }
    // Mark loading before scheduling the debounced fetch.
    // eslint-disable-next-line @eslint-react/set-state-in-effect
    setLoading(true);
    // eslint-disable-next-line @eslint-react/set-state-in-effect
    setError(null);
    let cancelled = false;
    const handle = window.setTimeout(async () => {
      try {
        const res = await api.search(trimmed);
        if (cancelled) return;
        setHits(res.hits);
        setActive(0);
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [query]);

  const openHit = (hit: SearchHit) => {
    // Hash carries the target message id so the chat route can scroll
    // to it after history loads. Plain location.hash beats a router
    // search param here: it survives reloads, doesn't show up in the
    // address bar as state, and pairs with `data-msg-id` on each row.
    navigate({
      to: "/c/$id",
      params: { id: hit.conv_id },
      hash: `m-${hit.message_id}`,
    });
    onClose();
  };

  const onInputKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(hits.length - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const hit = hits[active];
      if (hit) openHit(hit);
    }
  };

  return createPortal(
    <div
      role="dialog"
      aria-modal="true"
      aria-label="search conversations"
      onClick={onClose}
      css={{
        position: "fixed",
        inset: 0,
        zIndex: 60,
        background: "rgba(0, 0, 0, 0.45)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        padding: "12vh 16px 16px",
        [mq[0]]: { padding: "8vh 12px 12px" },
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        css={{
          width: "100%",
          maxWidth: 640,
          background: theme.colors.background.main,
          border: `1px solid ${theme.colors.border}`,
          borderRadius: theme.border.radius,
          boxShadow: theme.shadows.main,
          display: "flex",
          flexDirection: "column",
          maxHeight: "70vh",
          overflow: "hidden",
        }}
      >
        <div
          css={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "10px 14px",
            borderBottom: `1px solid ${theme.colors.border}`,
          }}
        >
          <span
            className="material-icons-outlined"
            aria-hidden
            css={{ fontSize: 20, color: theme.colors.text.muted }}
          >
            search
          </span>
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onInputKey}
            placeholder="search every conversation…"
            autoComplete="off"
            spellCheck={false}
            css={{
              ...theme.typography.body1,
              flex: 1,
              border: "none",
              outline: "none",
              background: "transparent",
              color: theme.colors.text.main,
              "&::placeholder": { color: theme.colors.text.muted },
            }}
          />
          <kbd
            aria-hidden
            css={{
              ...theme.typography.caption,
              color: theme.colors.text.muted,
              padding: "1px 6px",
              border: `1px solid ${theme.colors.border}`,
              borderRadius: 4,
              fontFamily:
                "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
              fontSize: 11,
            }}
          >
            esc
          </kbd>
        </div>
        <div
          css={{
            flex: 1,
            overflowY: "auto",
          }}
        >
          {error && (
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.error,
                padding: "14px 16px",
              }}
            >
              {error}
            </div>
          )}
          {!error && query.trim() && !loading && hits.length === 0 && (
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.text.muted,
                padding: "14px 16px",
              }}
            >
              no matches
            </div>
          )}
          {!error && !query.trim() && (
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.text.muted,
                padding: "14px 16px",
              }}
            >
              type to search your conversations. ↑ ↓ move, enter to open.
            </div>
          )}
          {hits.map((hit, idx) => (
            <HitRow
              key={hit.message_id}
              hit={hit}
              active={idx === active}
              onClick={() => openHit(hit)}
              onHover={() => setActive(idx)}
            />
          ))}
        </div>
      </div>
    </div>,
    document.body,
  );
};

const HitRow = ({
  hit,
  active,
  onClick,
  onHover,
}: {
  hit: SearchHit;
  active: boolean;
  onClick: () => void;
  onHover: () => void;
}) => {
  const theme = useTheme();
  return (
    <button
      type="button"
      onClick={onClick}
      onMouseEnter={onHover}
      css={{
        width: "100%",
        textAlign: "left",
        background: active ? theme.colors.activity.onSoft : "transparent",
        border: "none",
        borderLeft: `2px solid ${
          active ? theme.colors.activity.on : "transparent"
        }`,
        padding: "10px 14px",
        cursor: "pointer",
        display: "flex",
        flexDirection: "column",
        gap: 4,
        color: theme.colors.text.main,
      }}
    >
      <div
        css={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
        }}
      >
        <span
          css={{
            ...theme.typography.body2,
            fontFamily: theme.fonts.heading,
            color: theme.colors.text.main,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
            flex: 1,
          }}
        >
          {hit.conv_title || "untitled"}
        </span>
        <span
          css={{
            ...theme.typography.caption,
            color: theme.colors.text.muted,
            flexShrink: 0,
          }}
        >
          {hit.role === "user" ? "you" : "assistant"}
        </span>
      </div>
      <div
        css={{
          ...theme.typography.caption,
          color: theme.colors.text.muted,
          lineHeight: 1.4,
        }}
      >
        {renderSnippet(hit.snippet, theme.colors.activity.on)}
      </div>
    </button>
  );
};

/// Convert FTS5 snippet markers `[term]` into highlighted spans. The
/// snippet text is run through the markdown stripper first so backticks
/// / asterisks from the source content don't leak through as literal
/// characters in the result list.
const renderSnippet = (snippet: string, accent: string) => {
  const parts = stripMarkdown(snippet).split(/(\[[^\]]+\])/g);
  // The snippet is a flat list of static fragments derived from the
  // upstream snippet text; index keys are safe — they don't reorder.
  return parts.map((part, i) => {
    if (part.startsWith("[") && part.endsWith("]")) {
      return (
        <mark
          // eslint-disable-next-line @eslint-react/no-array-index-key
          key={i}
          css={{
            background: "transparent",
            color: accent,
            fontWeight: 500,
          }}
        >
          {part.slice(1, -1)}
        </mark>
      );
    }
    // eslint-disable-next-line @eslint-react/no-array-index-key
    return <Fragment key={i}>{part}</Fragment>;
  });
};

export default SearchPalette;
