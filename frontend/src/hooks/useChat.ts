import { useCallback, useEffect, useRef, useState } from "react";
import { useSWRConfig } from "swr";

import { api, imageUrl, maskUrl, Message, streamChat } from "../api";

/**
 * Fetch a persisted message's base images as base64 (no `data:`
 * prefix). Used by the resend paths (edit, regenerate, regenerate-
 * from-user) — message list payloads only carry `image_count`, so
 * the bytes have to come through the per-image endpoint. Returns
 * `undefined` when the row has no images so the caller can pass the
 * result straight through to `send`.
 */
const fetchPersistedImages = async (
  convId: string,
  msgId: number,
  count: number,
): Promise<string[] | undefined> => {
  if (!count) return undefined;
  return Promise.all(
    Array.from({ length: count }, async (_, i) => {
      const blob = await (
        await fetch(imageUrl(convId, msgId, i), { credentials: "include" })
      ).blob();
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result));
        r.onerror = () => reject(r.error);
        r.readAsDataURL(blob);
      });
      const comma = dataUrl.indexOf(",");
      return comma >= 0 ? dataUrl.slice(comma + 1) : "";
    }),
  );
};

/**
 * Fetch a persisted message's inpaint mask if it carries one.
 * Returns `undefined` on 404 or any failure — the caller treats that
 * as "no mask" and falls back to plain img2img/txt2img routing.
 */
const fetchPersistedMask = async (
  convId: string,
  msgId: number,
): Promise<string | undefined> => {
  try {
    const res = await fetch(maskUrl(convId, msgId), {
      credentials: "include",
    });
    if (!res.ok) return undefined;
    const blob = await res.blob();
    const dataUrl = await new Promise<string>((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(String(r.result));
      r.onerror = () => reject(r.error);
      r.readAsDataURL(blob);
    });
    const comma = dataUrl.indexOf(",");
    return comma >= 0 ? dataUrl.slice(comma + 1) : undefined;
  } catch (e) {
    console.warn("mask fetch failed", e);
    return undefined;
  }
};

/**
 * Send arguments collapsed into an options bag so the route + every
 * internal resend path (edit, regenerate, regenerate-from-user) share
 * one shape. Earlier iterations took 10+ positional args which was a
 * recipe for off-by-one bugs when a new field landed.
 */
export type SendOptions = {
  content: string;
  model?: string;
  images?: string[];
  mode?: "chat" | "image";
  refine?: boolean;
  persona?: string;
  /** When set, drops the failed assistant row and re-runs generation
   * off the existing user message — no new user bubble appears. */
  retryAssistantId?: number;
  /** When set, trims everything after this user row and re-runs
   * generation off it. */
  regenerateFromUser?: number;
  /** Image-mode sub-routing. "inpaint" requires a mask + exactly one
   * image. */
  subMode?: "txt2img" | "img2img" | "inpaint";
  /** Base64 PNG mask. Only carried into the request when sub_mode is
   * inpaint. */
  mask?: string;
  /** Optional negative-prompt override. */
  negative?: string;
  /** Search the web before answering and answer from the results. Chat
   * mode only. */
  webSearch?: boolean;
};

type LiveMessage = Pick<Message, "role" | "content" | "sources"> & {
  id?: number;
  /** Optimistic-state base64 (no `data:` prefix) — only present before
   * the server round-trip; afterwards `image_count` carries the metadata
   * and the actual bytes are loaded via `imageUrl(...)`. */
  images?: string[];
  image_count?: number;
  status?: "done" | "pending" | "error";
  /** Live sampler progress while a ComfyUI img2img job is churning.
   * Cleared once the job lands (status flips to done/error). */
  progress?: { value: number; max: number };
  /** Latest preview frame from ComfyUI (data URL). Same lifecycle as
   * `progress` — only meaningful while `status === "pending"`. */
  previewDataUrl?: string;
  /** Image-mode only: backend is waiting on the image-gen semaphore.
   * Cleared automatically when the first progress / preview / delta
   * event arrives. */
  queued?: boolean;
  /** Image-mode only: jobs ahead of ours in the shared ComfyUI host's
   * queue (often another backend's work). Undefined when we're only
   * waiting on our own semaphore. Same lifecycle as `queued`. */
  queuedAhead?: number;
  /** Text-mode only: end-of-turn generation stats from Ollama. Not
   * persisted server-side — present only on the live stream of this
   * conversation. */
  stats?: { tokens: number; prompt_tokens: number; tokens_per_sec: number };
};

export function useChat(convId: string | undefined) {
  const { mutate } = useSWRConfig();
  const [messages, setMessages] = useState<LiveMessage[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [lastConvId, setLastConvId] = useState(convId);
  const abortRef = useRef<AbortController | null>(null);
  /// Server doesn't persist generation stats, so the in-memory copy on
  /// the last assistant bubble would be wiped when we re-fetch the
  /// canonical thread at end-of-stream. Stash the latest stats event
  /// here and splice it back onto the freshly-fetched row.
  const lastStatsRef = useRef<LiveMessage["stats"] | null>(null);

  // Reset during render when the conversation switches — keeps initial
  // state synchronized with the prop without an effect.
  if (lastConvId !== convId) {
    setLastConvId(convId);
    setMessages([]);
    setError(null);
    setLoaded(false);
    lastStatsRef.current = null;
  }

  useEffect(() => {
    if (!convId) return;
    let cancelled = false;
    api.getMessages(convId).then(
      (rows) => {
        if (cancelled) return;
        setMessages(rows);
        setLoaded(true);
      },
      (e: unknown) => {
        if (cancelled) return;
        setError(String(e));
        setLoaded(true);
      },
    );
    return () => {
      cancelled = true;
      abortRef.current?.abort();
      abortRef.current = null;
    };
  }, [convId]);

  const send = useCallback(
    async (opts: SendOptions) => {
      if (!convId || streaming) return;
      const {
        content,
        model,
        images,
        mode,
        refine,
        persona,
        retryAssistantId,
        regenerateFromUser,
        subMode,
        mask,
        negative,
        webSearch,
      } = opts;
      const controller = new AbortController();
      abortRef.current = controller;
      let aborted = false;
      setStreaming(true);
      setError(null);
      setMessages((prev) => {
        if (retryAssistantId != null) {
          // Reuse the existing user bubble; just replace the failed
          // assistant row with a fresh pending placeholder.
          const filtered = prev.filter((m) => m.id !== retryAssistantId);
          return [
            ...filtered,
            {
              role: "assistant",
              content: "",
              status: mode === "image" ? "pending" : "done",
            },
          ];
        }
        if (regenerateFromUser != null) {
          // Keep history up to and including this user row, drop
          // everything after, append a fresh assistant placeholder.
          const idx = prev.findIndex((m) => m.id === regenerateFromUser);
          if (idx === -1) return prev;
          return [
            ...prev.slice(0, idx + 1),
            {
              role: "assistant",
              content: "",
              status: mode === "image" ? "pending" : "done",
            },
          ];
        }
        return [
          ...prev,
          { role: "user", content, images },
          {
            role: "assistant",
            content: "",
            status: mode === "image" ? "pending" : "done",
          },
        ];
      });

      try {
        for await (const evt of streamChat(
          {
            conv_id: convId,
            content,
            model,
            images,
            mode,
            refine,
            persona,
            retry_assistant_id: retryAssistantId,
            regenerate_from_user: regenerateFromUser,
            sub_mode: subMode,
            mask,
            negative,
            web_search: webSearch,
          },
          controller.signal,
        )) {
          if (evt.type === "delta") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                // Spread, don't rebuild: `context` lands before the
                // first delta, so a fresh object here would drop the
                // sources chip the moment the answer starts streaming.
                next[next.length - 1] = {
                  ...last,
                  content: last.content + evt.content,
                };
              }
              return next;
            });
          } else if (evt.type === "queued") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = {
                  ...last,
                  queued: true,
                  queuedAhead: evt.ahead,
                };
              }
              return next;
            });
          } else if (evt.type === "progress") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = {
                  ...last,
                  queued: false,
                  queuedAhead: undefined,
                  progress: { value: evt.value, max: evt.max },
                };
              }
              return next;
            });
          } else if (evt.type === "preview") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = {
                  ...last,
                  queued: false,
                  queuedAhead: undefined,
                  previewDataUrl: `data:${evt.mime};base64,${evt.b64}`,
                };
              }
              return next;
            });
          } else if (evt.type === "context") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = { ...last, sources: evt.sources };
              }
              return next;
            });
          } else if (evt.type === "stats") {
            const stats = {
              tokens: evt.tokens,
              prompt_tokens: evt.prompt_tokens,
              tokens_per_sec: evt.tokens_per_sec,
            };
            lastStatsRef.current = stats;
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = { ...last, stats };
              }
              return next;
            });
          } else if (evt.type === "done") {
            // The backend kicks off an auto-rename task after emitting
            // `done`. Re-fetch the sidebar list once it has likely
            // settled so the new title appears without a manual reload.
            setTimeout(() => {
              void mutate("/api/conversations");
            }, 3500);
          } else if (evt.type === "error") {
            setError(evt.message);
          }
        }
      } catch (e) {
        // AbortError is expected when the user presses stop — not a failure.
        if (e instanceof DOMException && e.name === "AbortError") {
          aborted = true;
        } else {
          setError(String(e));
        }
      } finally {
        setStreaming(false);
        abortRef.current = null;
        await mutate("/api/conversations");
        // Refresh thread from the server: backend persists whatever was
        // streamed (even on stop), so the canonical version replaces the
        // optimistic in-memory one. The persisted error row (image-mode
        // failures hit `fail_message`) carries the same text we already
        // wrote into top-level `error` from the SSE `error` event — clear
        // the transient state so we don't render the message twice.
        //
        // On abort we skip the immediate fetch: the backend cancel chain
        // (interrupt comfyui + delete pending row) runs async and can
        // take a few seconds, so a fetch right now would re-hydrate the
        // pending row we just optimistically removed in `stop`. The
        // pending-row poll effect picks up any leftover state if the
        // backend ever fails to clean up.
        if (convId && !aborted) {
          api.getMessages(convId).then(
            (rows) => {
              // Stats live only on the in-memory copy; re-attach them
              // to the freshly-fetched final assistant row so the
              // caption survives the post-stream refetch.
              const pending = lastStatsRef.current;
              if (pending && rows.length > 0) {
                const lastIdx = rows.length - 1;
                if (rows[lastIdx].role === "assistant") {
                  (rows[lastIdx] as LiveMessage).stats = pending;
                }
              }
              lastStatsRef.current = null;
              setMessages(rows);
              if (rows.some((r) => r.status === "error")) {
                setError(null);
              }
            },
            () => {},
          );
        }
      }
    },
    [convId, streaming, mutate],
  );

  const stop = useCallback(() => {
    abortRef.current?.abort();
    // Drop the placeholder bubble immediately. The backend cancel chain
    // (interrupt comfyui + delete pending row) is in flight; deleting
    // optimistically here means the UI isn't stuck behind that latency.
    // Text-chat rows don't carry a `pending` status — they just sit
    // empty until deltas arrive — so we also drop a trailing assistant
    // with no content yet so the spinner doesn't survive the abort.
    setMessages((prev) => {
      if (prev.length === 0) return prev;
      const last = prev[prev.length - 1];
      if (last.role !== "assistant") return prev;
      if (last.status === "pending" || !last.content) {
        return prev.slice(0, -1);
      }
      return prev;
    });
  }, []);

  /**
   * Cancel an in-flight image generation whose original SSE connection is
   * not held by this tab. Used after a reload (or when the user opens the
   * conversation in a second tab) where `streaming` is false but a
   * `pending` row is still present. Posts to the server's cancel endpoint
   * which interrupts ComfyUI and drops the placeholder row, then refreshes
   * the local thread.
   */
  const cancelPending = useCallback(
    async (messageId: number) => {
      if (!convId) return;
      try {
        await api.cancelPending(convId, messageId);
        const rows = await api.getMessages(convId);
        setMessages(rows);
      } catch (e) {
        setError(String(e));
      }
    },
    [convId],
  );

  const deleteFrom = useCallback(
    async (messageId: number) => {
      if (!convId) return;
      try {
        await api.deleteMessageFrom(convId, messageId);
        const rows = await api.getMessages(convId);
        setMessages(rows);
        await mutate("/api/conversations");
      } catch (e) {
        setError(String(e));
      }
    },
    [convId, mutate],
  );

  const editAndResend = useCallback(
    async (
      messageId: number,
      newContent: string,
      model?: string,
      mode?: "chat" | "image",
      refine?: boolean,
      persona?: string,
    ) => {
      if (!convId || streaming) return;
      const idx = messages.findIndex((m) => m.id === messageId);
      if (idx === -1) return;
      const target = messages[idx];
      if (target.role !== "user") return;

      // Carry the original attachments so the resend is a true edit, not
      // an attachment-stripping reset. Optimistic rows already hold base64;
      // persisted rows hand them out lazily through `imageUrl(...)`.
      let images: string[] | undefined;
      if (target.images && target.images.length > 0) {
        images = target.images;
      } else if (target.image_count && target.image_count > 0) {
        try {
          images = await fetchPersistedImages(
            convId,
            messageId,
            target.image_count,
          );
        } catch (e) {
          setError(String(e));
          return;
        }
      }

      // Pull the mask if the original turn carried one — backend
      // routing keys on its presence. 404 = no mask, soft fallback.
      const mask = await fetchPersistedMask(convId, messageId);

      try {
        await api.deleteMessageFrom(convId, messageId);
      } catch (e) {
        setError(String(e));
        return;
      }
      // Trim the local thread to match the truncate. `send` re-appends an
      // optimistic user bubble right after, so the UI never flashes empty.
      setMessages((prev) => prev.slice(0, idx));
      await send({
        content: newContent,
        model,
        images,
        mode,
        refine,
        persona,
        subMode: mask ? "inpaint" : undefined,
        mask,
      });
    },
    [convId, streaming, messages, send],
  );

  const regenerateFromUser = useCallback(
    async (
      userMessageId: number,
      model?: string,
      modeOverride?: "chat" | "image",
    ) => {
      if (!convId || streaming) return;
      const idx = messages.findIndex((m) => m.id === userMessageId);
      if (idx === -1) return;
      const target = messages[idx];
      if (target.role !== "user") return;
      // Prefer the explicit override (the route passes one informed by
      // model caps). Otherwise sniff mode from the assistant reply
      // that follows — image-mode rows carry attachments or have
      // status === "error". Default fallback: plain chat.
      let inferredMode: "chat" | "image" = "chat";
      if (modeOverride) {
        inferredMode = modeOverride;
      } else {
        const next = messages[idx + 1];
        if (next && next.role === "assistant") {
          const nextImg = (next.image_count ?? 0) + (next.images?.length ?? 0);
          inferredMode =
            next.status === "error" || nextImg > 0 ? "image" : "chat";
        }
      }
      // Persisted user rows only carry an `image_count` — bytes live
      // behind /api/.../image/:idx. Hand them back to the backend
      // explicitly so the inpaint sub-mode router can match
      // `images.len() == 1 && mask.is_some()`.
      let images = target.images;
      if (!images && target.image_count) {
        try {
          images = await fetchPersistedImages(
            convId,
            userMessageId,
            target.image_count,
          );
        } catch (e) {
          setError(String(e));
          return;
        }
      }
      const mask = await fetchPersistedMask(convId, userMessageId);
      await send({
        content: target.content,
        model,
        images,
        mode: inferredMode,
        regenerateFromUser: userMessageId,
        subMode: mask ? "inpaint" : undefined,
        mask,
      });
    },
    [convId, streaming, messages, send],
  );

  const regenerate = useCallback(
    async (assistantId: number) => {
      if (!convId || streaming) return;
      const idx = messages.findIndex((m) => m.id === assistantId);
      if (idx === -1) return;
      let userIdx = -1;
      for (let i = idx - 1; i >= 0; i--) {
        if (messages[i].role === "user") {
          userIdx = i;
          break;
        }
      }
      if (userIdx === -1) return;
      const prior = messages[userIdx];
      const target = messages[idx];
      const targetImageCount =
        (target.image_count ?? 0) + (target.images?.length ?? 0);
      // Error rows only ever come from the image branch (chat-mode
      // failures aren't persisted), so retry them as image regardless of
      // attachment count. For done rows, presence of images is the tell.
      const inferredMode: "chat" | "image" =
        target.status === "error" || targetImageCount > 0 ? "image" : "chat";
      // Backend deletes the failed assistant row inside the same /api/chat
      // request; no client-side delete + re-fetch round trip needed, and
      // no second user bubble appears. Re-fetch the prior user row's
      // base images + mask so an inpaint retry doesn't bounce with
      // "inpaint requires exactly one base image" — DTO state only
      // carries image_count + has_mask, never the bytes themselves.
      let images = prior.images;
      let mask: string | undefined;
      if (typeof prior.id === "number") {
        if (!images && prior.image_count) {
          try {
            images = await fetchPersistedImages(
              convId,
              prior.id,
              prior.image_count,
            );
          } catch (e) {
            setError(String(e));
            return;
          }
        }
        mask = await fetchPersistedMask(convId, prior.id);
      }
      await send({
        content: prior.content,
        images,
        mode: inferredMode,
        retryAssistantId: assistantId,
        subMode: mask ? "inpaint" : undefined,
        mask,
      });
    },
    [convId, messages, send, streaming],
  );

  // Poll while any message is still pending (e.g. an image generation
  // started in another tab or before a reload). Skipped while a live SSE
  // stream is running — that path drives state via deltas + progress +
  // preview events, and overwriting it with bare server rows every 4 s
  // wipes the SSE-only fields (`progress`, `previewDataUrl`).
  const hasPending = messages.some((m) => m.status === "pending");
  useEffect(() => {
    if (!convId || !hasPending || streaming) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const rows = await api.getMessages(convId);
        if (cancelled) return;
        setMessages(rows);
      } catch {
        // ignore — next tick will retry
      }
    };
    const id = window.setInterval(() => void tick(), 4000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [convId, hasPending, streaming]);

  return {
    messages,
    streaming,
    error,
    loaded,
    send,
    stop,
    cancelPending,
    deleteFrom,
    regenerate,
    regenerateFromUser,
    editAndResend,
  };
}
