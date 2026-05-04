/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createFileRoute, useParams } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import useSWR from "swr";

import { api, Conversation, ModelCapabilities } from "../api";
import Composer from "../components/Composer";
import MessageView from "../components/MessageView";
import { useChat } from "../hooks/useChat";
import { mq } from "../mq";

const LAST_MODEL_KEY = "chat:lastModel";

const STICK_THRESHOLD_PX = 60;

const ChatView = () => {
  const theme = useTheme();
  const { id } = useParams({ from: "/c/$id" });
  const scrollRef = useRef<HTMLDivElement>(null);
  // True while the viewport is glued to the bottom; flipped off the moment
  // the user scrolls up so streaming deltas don't yank them back.
  const stickRef = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const { messages, streaming, error, send, stop } = useChat(id);

  const { data: conversations } = useSWR<Conversation[]>(
    "/api/conversations",
    api.listConversations,
  );
  const conv = useMemo(
    () => conversations?.find((c) => c.id === id),
    [conversations, id],
  );

  // Per-conversation model state. Seeded from the conversation row on
  // load; falls back to whatever the user last picked in this browser
  // (localStorage) for fresh chats.
  const [model, setModel] = useState<string | null>(null);
  const [lastModelKey, setLastModelKey] = useState<string>("");
  const modelKey = `${id}|${conv?.model ?? ""}`;
  if (lastModelKey !== modelKey) {
    setLastModelKey(modelKey);
    if (conv?.model) {
      setModel(conv.model);
    } else {
      try {
        setModel(window.localStorage.getItem(LAST_MODEL_KEY));
      } catch {
        setModel(null);
      }
    }
  }

  // Available models on the server — used to reconcile the seeded model
  // against the live list. Stale localStorage entries (e.g. a model that
  // was pulled or renamed upstream) would otherwise leave the picker
  // pointing at a non-existent model.
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

  const onModelChange = (next: string) => {
    setModel(next);
    try {
      window.localStorage.setItem(LAST_MODEL_KEY, next);
    } catch {
      // ignore storage errors (private mode, quota)
    }
  };

  const scrollToBottom = (behavior: ScrollBehavior = "auto") => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior });
  };

  // Caps for the active model — drives whether the + image-attach button
  // shows up. SWR keys per model so switching is instant after first fetch.
  const { data: caps } = useSWR<ModelCapabilities>(
    model ? ["caps", model] : null,
    () => api.modelCaps(model as string),
  );

  const sendWithModel = (content: string, images?: string[]) => {
    // The user just hit send — they expect to see their message and the
    // incoming reply. Re-glue to the bottom regardless of where they were.
    stickRef.current = true;
    setShowJump(false);
    void send(content, model ?? undefined, images);
  };

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    const atBottom = distance < STICK_THRESHOLD_PX;
    stickRef.current = atBottom;
    setShowJump(!atBottom && streaming);
  };

  // newest-first list of past user prompts, fed to the composer for
  // ↑/↓ shell-style recall. Filter out the optimistic in-flight echo
  // (the most recent user message is already in-thread; that's fine —
  // ArrowUp pulls it back instantly for editing).
  const userHistory = useMemo(
    () =>
      messages
        .filter((m) => m.role === "user" && m.content.trim().length > 0)
        .map((m) => m.content)
        .reverse(),
    [messages],
  );

  // On conversation switch: force-glue and scroll to bottom once the
  // initial messages render. Reset the UI flags during render; the actual
  // scroll has to wait for layout, so it stays in an effect.
  const [lastGlueId, setLastGlueId] = useState(id);
  if (lastGlueId !== id) {
    setLastGlueId(id);
    stickRef.current = true;
    setShowJump(false);
  }
  useEffect(() => {
    scrollToBottom("auto");
  }, [id]);

  // On message updates: only follow if the user is already pinned to the
  // bottom. Otherwise leave their scroll position alone.
  const lastMessageContent = messages[messages.length - 1]?.content;
  useEffect(() => {
    if (stickRef.current) {
      scrollToBottom("auto");
    } else if (streaming) {
      setShowJump(true);
    }
  }, [messages.length, lastMessageContent, streaming]);

  return (
    <>
      <div
        ref={scrollRef}
        onScroll={onScroll}
        css={{
          flex: 1,
          overflowY: "auto",
          position: "relative",
          // top padding clears the floating hamburger button (when sidebar
          // is collapsed). Content scrolls behind the button by design.
          padding: "60px 24px 24px",
          [mq[0]]: { padding: "60px 16px 16px" },
        }}
      >
        <div css={{ maxWidth: 760, margin: "0 auto" }}>
          {messages.map((m, i) => (
            // append-only list; index is stable.
            // eslint-disable-next-line @eslint-react/no-array-index-key
            <MessageView key={i} msg={m} />
          ))}
          {error && (
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.error,
                textAlign: "center",
                padding: 12,
              }}
            >
              {error}
            </div>
          )}
        </div>
      </div>
      {showJump && (
        <button
          type="button"
          aria-label="jump to latest"
          onClick={() => {
            stickRef.current = true;
            setShowJump(false);
            scrollToBottom("smooth");
          }}
          css={{
            position: "absolute",
            bottom: 96,
            left: "50%",
            transform: "translateX(-50%)",
            width: 36,
            height: 36,
            borderRadius: "50%",
            border: `1px solid ${theme.colors.border}`,
            background: theme.colors.background.main,
            color: theme.colors.text.main,
            cursor: "pointer",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            boxShadow: theme.shadows.main,
            zIndex: 5,
            "&:hover": { background: theme.colors.background.light },
          }}
        >
          <span className="material-icons-outlined" css={{ fontSize: 22 }}>
            arrow_downward
          </span>
        </button>
      )}
      <Composer
        onSend={sendWithModel}
        streaming={streaming}
        onStop={stop}
        history={userHistory}
        model={model}
        onModelChange={onModelChange}
        vision={caps?.vision ?? false}
      />
    </>
  );
};

export const Route = createFileRoute("/c/$id")({
  component: ChatView,
});
