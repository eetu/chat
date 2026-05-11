/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createFileRoute, useParams } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import useSWR from "swr";

import { api, Conversation, ModelCapabilities, Persona, Status } from "../api";
import Composer, { ComposerHandle } from "../components/Composer";
import MessageView from "../components/MessageView";
import { useChat } from "../hooks/useChat";
import { mq } from "../mq";

const LAST_MODEL_KEY = "chat:lastModel";

// Re-glue when the user scrolls back within this many pixels of the bottom.
// Kept tight so any deliberate upward scroll releases the follow.
const REGLUE_THRESHOLD_PX = 4;

const ChatView = () => {
  const theme = useTheme();
  const { id } = useParams({ from: "/c/$id" });
  const scrollRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<ComposerHandle>(null);
  // True while the viewport is glued to the bottom; flipped off the moment
  // the user scrolls up so streaming deltas don't yank them back.
  const stickRef = useRef(true);
  const prevScrollTopRef = useRef(0);
  const [showJump, setShowJump] = useState(false);
  const {
    messages,
    streaming,
    error,
    loaded,
    send,
    stop,
    cancelPending,
    deleteFrom,
    regenerate,
    editAndResend,
  } = useChat(id);

  // A pending assistant row means an image generation is still churning —
  // either in this tab's SSE channel (streaming === true) or kicked off
  // earlier in another tab / pre-reload (streaming === false but the row
  // is persisted server-side). We treat both as "busy" so the composer's
  // stop button stays available, and route the stop action to the right
  // handler: AbortController for live SSE, cancel API for the orphaned case.
  const pendingMsg = messages.find((m) => m.status === "pending");
  const busy = streaming || !!pendingMsg;
  const onStopBusy = () => {
    if (streaming) {
      stop();
    } else if (pendingMsg?.id != null) {
      void cancelPending(pendingMsg.id);
    }
  };

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

  const { data: status } = useSWR<Status>("/status", api.status);
  const { data: personas } = useSWR<Persona[]>(
    status?.refiner_available ? "/api/personas" : null,
    api.personas,
  );

  const sendWithModel = (
    content: string,
    images?: string[],
    mode?: "chat" | "image",
    refine?: boolean,
    persona?: string,
  ) => {
    // The user just hit send — they expect to see their message and the
    // incoming reply. Re-glue to the bottom regardless of where they were.
    stickRef.current = true;
    setShowJump(false);
    void send(content, model ?? undefined, images, mode, refine, persona);
  };

  /**
   * Edit a past user message and re-run from there. Truncates everything
   * after that row, then sends the new content with the original images
   * carried forward. Mode is inferred from attachments — same heuristic
   * the regenerate path uses.
   */
  const onEdit = (id: number, newContent: string) => {
    const idx = messages.findIndex((m) => m.id === id);
    if (idx === -1) return;
    const target = messages[idx];
    const imgCount = (target.image_count ?? 0) + (target.images?.length ?? 0);
    const mode: "chat" | "image" | undefined =
      imgCount > 0 ? "image" : undefined;
    stickRef.current = true;
    setShowJump(false);
    void editAndResend(id, newContent, model ?? undefined, mode);
  };

  /**
   * Pull an existing generated image into the composer as the seed for a
   * new img2img turn. Accepts either a data URL (optimistic state) or a
   * cookie-authed `/api/conversations/.../image/...` URL.
   */
  const onRemix = async (src: string) => {
    try {
      let base64: string;
      let preview: string;
      if (src.startsWith("data:")) {
        preview = src;
        const comma = src.indexOf(",");
        base64 = comma >= 0 ? src.slice(comma + 1) : "";
      } else {
        const blob = await (
          await fetch(src, { credentials: "include" })
        ).blob();
        preview = await new Promise<string>((resolve, reject) => {
          const r = new FileReader();
          r.onload = () => resolve(String(r.result));
          r.onerror = () => reject(r.error);
          r.readAsDataURL(blob);
        });
        const comma = preview.indexOf(",");
        base64 = comma >= 0 ? preview.slice(comma + 1) : "";
      }
      if (!base64) return;
      composerRef.current?.remixWithImage({ base64, preview });
    } catch (e) {
      console.error("remix failed", e);
    }
  };

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const direction = el.scrollTop - prevScrollTopRef.current;
    prevScrollTopRef.current = el.scrollTop;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    const atBottom = distance < REGLUE_THRESHOLD_PX;
    // atBottom wins over direction so bounce/rubber-band events that
    // briefly fire a negative-direction scroll while the viewport is
    // still pinned at the bottom don't flicker stick off → on.
    if (atBottom) {
      stickRef.current = true;
    } else if (direction < 0) {
      stickRef.current = false;
    }
    setShowJump(!stickRef.current && streaming);
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

  // Drain a pending draft handed off from the landing page. Runs once per
  // conversation (consumedRef guard) after history has loaded — sending
  // earlier would race the empty getMessages response that overwrites
  // optimistic state.
  const consumedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!id || !loaded) return;
    if (consumedRef.current === id) return;
    consumedRef.current = id;

    type Pending = {
      content: string;
      images?: string[];
      model?: string | null;
      mode?: "chat" | "image";
      refine?: boolean;
      persona?: string;
    };
    const parsed = ((): Pending | null => {
      try {
        const raw = window.sessionStorage.getItem(`chat:pending:${id}`);
        if (!raw) return null;
        window.sessionStorage.removeItem(`chat:pending:${id}`);
        return JSON.parse(raw) as Pending;
      } catch {
        return null;
      }
    })();
    if (!parsed) return;

    // No setModel here — conv.model (already passed at create time) syncs
    // into the picker via the modelKey block above. Setting it here too
    // would duplicate the update and trip eslint's set-state-in-effect rule.
    stickRef.current = true;
    void send(
      parsed.content,
      parsed.model ?? undefined,
      parsed.images,
      parsed.mode,
      parsed.refine,
      parsed.persona,
    );
  }, [id, loaded, send]);

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
            <MessageView
              key={m.id ?? `optimistic-${i}`}
              msg={m}
              convId={id}
              onDeleteFrom={deleteFrom}
              onRegenerate={regenerate}
              onRemix={status?.img2img_available ? onRemix : undefined}
              onEdit={onEdit}
              busy={busy}
            />
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
      <div
        css={{
          padding: "0px 8px 16px",
        }}
      >
        <Composer
          ref={composerRef}
          onSend={sendWithModel}
          streaming={busy}
          onStop={onStopBusy}
          history={userHistory}
          model={model}
          onModelChange={onModelChange}
          vision={caps?.vision ?? false}
          chatCap={caps?.chat ?? true}
          imageGen={caps?.image_gen ?? false}
          refinerAvailable={status?.refiner_available ?? false}
          img2imgAvailable={status?.img2img_available ?? false}
          personas={personas}
        />
      </div>
    </>
  );
};

export const Route = createFileRoute("/c/$id")({
  component: ChatView,
});
