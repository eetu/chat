export type Conversation = {
  id: string;
  title: string;
  model: string | null;
  created_at: number;
  updated_at: number;
};

export type Message = {
  id: number;
  role: "user" | "assistant" | "system";
  content: string;
  created_at: number;
  /** Count of image attachments. Bytes are fetched on demand via
   * `imageUrl(convId, id, idx)` rather than inlined in the list payload. */
  image_count?: number;
  status?: "done" | "pending" | "error";
};

export const imageUrl = (convId: string, msgId: number, idx: number) =>
  `/api/conversations/${convId}/messages/${msgId}/image/${idx}`;

export type ModelCapabilities = {
  vision: boolean;
  tools: boolean;
  chat: boolean;
  image_gen: boolean;
  capabilities: string[];
  families: string[];
};

export type Me = { sub: string; username: string };

export type Status = {
  upstream: boolean;
  model_locked: boolean;
  auth: "dev" | "oidc" | "none";
  refiner_available: boolean;
  img2img_available: boolean;
  voice_in_available: boolean;
  voice_out_available: boolean;
};

export type Persona = {
  id: string;
  label: string;
  description: string;
};

export type SearchHit = {
  message_id: number;
  conv_id: string;
  conv_title: string;
  role: "user" | "assistant" | "system";
  created_at: number;
  /** FTS5 snippet with `[…]` markers around matched terms. */
  snippet: string;
};

const json = async <T>(res: Response): Promise<T> => {
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${text}`);
  }
  return res.json() as Promise<T>;
};

export const api = {
  me: () => fetch("/api/me", { credentials: "include" }).then(json<Me>),
  status: () => fetch("/status").then(json<Status>),
  personas: () =>
    fetch("/api/personas", { credentials: "include" }).then(json<Persona[]>),
  models: () =>
    fetch("/api/models", { credentials: "include" }).then(
      json<{ models?: Array<{ name: string; locked?: boolean }> }>,
    ),
  listConversations: () =>
    fetch("/api/conversations", { credentials: "include" }).then(
      json<Conversation[]>,
    ),
  createConversation: (body?: { title?: string; model?: string }) =>
    fetch("/api/conversations", {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body ?? {}),
    }).then(json<Conversation>),
  deleteConversation: (id: string) =>
    fetch(`/api/conversations/${id}`, {
      method: "DELETE",
      credentials: "include",
    }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
  renameConversation: (id: string, title: string) =>
    fetch(`/api/conversations/${id}`, {
      method: "PATCH",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ title }),
    }).then(json<Conversation>),
  getMessages: (id: string) =>
    fetch(`/api/conversations/${id}/messages`, {
      credentials: "include",
    }).then(json<Message[]>),
  deleteMessageFrom: (convId: string, msgId: number) =>
    fetch(`/api/conversations/${convId}/messages/${msgId}`, {
      method: "DELETE",
      credentials: "include",
    }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
  cancelPending: (convId: string, msgId: number) =>
    fetch(`/api/conversations/${convId}/messages/${msgId}/cancel`, {
      method: "POST",
      credentials: "include",
    }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
  deleteMe: () =>
    fetch("/api/me", { method: "DELETE", credentials: "include" }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
  modelCaps: (model: string) =>
    fetch(`/api/models/caps?model=${encodeURIComponent(model)}`, {
      credentials: "include",
    }).then(json<ModelCapabilities>),
  voices: () =>
    fetch("/api/voices", { credentials: "include" }).then(
      json<{ voices?: Array<string | { name?: string; voice?: string }> }>,
    ),
  search: (q: string) =>
    fetch(`/api/search?q=${encodeURIComponent(q)}`, {
      credentials: "include",
    }).then(json<{ hits: SearchHit[] }>),
};

export type ChatStreamEvent =
  | { type: "delta"; content: string }
  | { type: "done"; conv_id: string }
  | { type: "error"; message: string }
  | { type: "progress"; value: number; max: number }
  | { type: "preview"; mime: string; b64: string }
  /** Image-mode only: the backend is waiting on a slot in the image-gen
   * semaphore. Sent at most once per request and only when the permit
   * wasn't immediately available. */
  | { type: "queued" }
  /** Text-mode only: end-of-turn generation stats. Lives only for the
   * current SSE stream — not persisted yet, so reload drops them. */
  | {
      type: "stats";
      tokens: number;
      prompt_tokens: number;
      tokens_per_sec: number;
    };

/**
 * POST /api/chat and parse the SSE response stream produced by actix-web-lab.
 * Yields typed events; resolves when the stream ends.
 */
export async function* streamChat(
  body: {
    conv_id: string;
    content: string;
    model?: string;
    images?: string[];
    mode?: "chat" | "image";
    refine?: boolean;
    persona?: string;
    /** When set, the backend deletes this assistant row and re-runs
     * generation off the existing user turn instead of appending a new
     * one. Used by the in-bubble retry button. */
    retry_assistant_id?: number;
  },
  signal?: AbortSignal,
): AsyncGenerator<ChatStreamEvent> {
  const res = await fetch("/api/chat", {
    method: "POST",
    credentials: "include",
    headers: {
      "content-type": "application/json",
      accept: "text/event-stream",
    },
    body: JSON.stringify(body),
    signal,
  });
  if (!res.ok || !res.body) {
    throw new Error(`chat ${res.status}`);
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    // SSE frames are separated by \n\n
    let sep: number;
    while ((sep = buffer.indexOf("\n\n")) !== -1) {
      const frame = buffer.slice(0, sep);
      buffer = buffer.slice(sep + 2);
      const event = parseFrame(frame);
      if (event) yield event;
    }
  }
}

function parseFrame(frame: string): ChatStreamEvent | null {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of frame.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) {
      // SSE spec: strip exactly ONE optional leading space after `data:`.
      // Trimming all whitespace would eat leading spaces inside content
      // (e.g. word boundaries between streaming tokens).
      const rest = line.slice(5);
      dataLines.push(rest.startsWith(" ") ? rest.slice(1) : rest);
    }
  }
  if (dataLines.length === 0) return null;
  const data = dataLines.join("\n");

  if (event === "delta") return { type: "delta", content: data };
  if (event === "done") {
    try {
      const parsed = JSON.parse(data) as { conv_id: string };
      return { type: "done", conv_id: parsed.conv_id };
    } catch {
      return null;
    }
  }
  if (event === "error") {
    try {
      const parsed = JSON.parse(data) as { message: string };
      return { type: "error", message: parsed.message };
    } catch {
      return { type: "error", message: data };
    }
  }
  if (event === "progress") {
    try {
      const parsed = JSON.parse(data) as { value: number; max: number };
      return { type: "progress", value: parsed.value, max: parsed.max };
    } catch {
      return null;
    }
  }
  if (event === "queued") return { type: "queued" };
  if (event === "stats") {
    try {
      const parsed = JSON.parse(data) as {
        tokens: number;
        prompt_tokens: number;
        tokens_per_sec: number;
      };
      return {
        type: "stats",
        tokens: parsed.tokens,
        prompt_tokens: parsed.prompt_tokens,
        tokens_per_sec: parsed.tokens_per_sec,
      };
    } catch {
      return null;
    }
  }
  if (event === "preview") {
    try {
      const parsed = JSON.parse(data) as { mime: string; b64: string };
      return { type: "preview", mime: parsed.mime, b64: parsed.b64 };
    } catch {
      return null;
    }
  }
  return null;
}
