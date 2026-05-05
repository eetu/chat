import { useCallback, useEffect, useRef, useState } from "react";
import { useSWRConfig } from "swr";

import { api, Message, streamChat } from "../api";

type LiveMessage = Pick<Message, "role" | "content"> & { images?: string[] };

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
    async (content: string, model?: string, images?: string[]) => {
      if (!convId || streaming) return;
      const controller = new AbortController();
      abortRef.current = controller;
      setStreaming(true);
      setError(null);
      setMessages((prev) => [
        ...prev,
        { role: "user", content, images },
        { role: "assistant", content: "" },
      ]);

      try {
        for await (const evt of streamChat(
          { conv_id: convId, content, model, images },
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

  return { messages, streaming, error, loaded, send, stop };
}
