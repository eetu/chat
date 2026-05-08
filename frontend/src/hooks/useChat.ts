import { useCallback, useEffect, useRef, useState } from "react";
import { useSWRConfig } from "swr";

import { api, Message, streamChat } from "../api";

type LiveMessage = Pick<Message, "role" | "content"> & {
  id?: number;
  images?: string[];
  status?: "done" | "pending" | "error";
};

export function useChat(convId: string | undefined) {
  const { mutate } = useSWRConfig();
  const [messages, setMessages] = useState<LiveMessage[]>([]);
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [lastConvId, setLastConvId] = useState(convId);
  const abortRef = useRef<AbortController | null>(null);

  // Reset during render when the conversation switches — keeps initial
  // state synchronized with the prop without an effect.
  if (lastConvId !== convId) {
    setLastConvId(convId);
    setMessages([]);
    setError(null);
    setLoaded(false);
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
    async (
      content: string,
      model?: string,
      images?: string[],
      mode?: "chat" | "image",
      refine?: boolean,
    ) => {
      if (!convId || streaming) return;
      const controller = new AbortController();
      abortRef.current = controller;
      setStreaming(true);
      setError(null);
      setMessages((prev) => [
        ...prev,
        { role: "user", content, images },
        {
          role: "assistant",
          content: "",
          status: mode === "image" ? "pending" : "done",
        },
      ]);

      try {
        for await (const evt of streamChat(
          { conv_id: convId, content, model, images, mode, refine },
          controller.signal,
        )) {
          if (evt.type === "delta") {
            setMessages((prev) => {
              const next = prev.slice();
              const last = next[next.length - 1];
              if (last && last.role === "assistant") {
                next[next.length - 1] = {
                  role: "assistant",
                  content: last.content + evt.content,
                };
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
        if (!(e instanceof DOMException && e.name === "AbortError")) {
          setError(String(e));
        }
      } finally {
        setStreaming(false);
        abortRef.current = null;
        await mutate("/api/conversations");
        // Refresh thread from the server: backend persists whatever was
        // streamed (even on stop), so the canonical version replaces the
        // optimistic in-memory one.
        if (convId) {
          api.getMessages(convId).then(
            (rows) => setMessages(rows),
            () => {},
          );
        }
      }
    },
    [convId, streaming, mutate],
  );

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

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
      const inferredMode: "chat" | "image" =
        target.images && target.images.length > 0 ? "image" : "chat";
      try {
        await api.deleteMessageFrom(convId, assistantId);
        const rows = await api.getMessages(convId);
        setMessages(rows);
      } catch (e) {
        setError(String(e));
        return;
      }
      await send(prior.content, undefined, prior.images, inferredMode);
    },
    [convId, messages, send, streaming],
  );

  // Poll while any message is still pending (e.g. an image generation
  // started in another tab or before a reload). Stops as soon as the
  // refreshed list has no pending rows.
  const hasPending = messages.some((m) => m.status === "pending");
  useEffect(() => {
    if (!convId || !hasPending) return;
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
  }, [convId, hasPending]);

  return {
    messages,
    streaming,
    error,
    loaded,
    send,
    stop,
    deleteFrom,
    regenerate,
  };
}
