export type Conversation = {
  id: string;
  title: string;
  model: string | null;
  created_at: number;
  updated_at: number;
};

/** A retrieved source consulted for one assistant turn. `kind` picks
 * the icon and whether the pill is a link: `doc` entries come from the
 * user's uploaded documents and carry a cosine score, `web` entries come
 * from a live search and carry a URL. */
export type Source = {
  kind: "doc" | "web";
  name: string;
  url?: string;
  score?: number | null;
};

export type Message = {
  id: number;
  role: "user" | "assistant" | "system";
  content: string;
  created_at: number;
  /** Sources retrieved for this turn. Persisted server-side, so they
   * survive a reload. */
  sources?: Source[];
  /** Count of image attachments. Bytes are fetched on demand via
   * `imageUrl(convId, id, idx)` rather than inlined in the list payload. */
  image_count?: number;
  /** True when this row carries an inpaint mask, fetchable via
   * `maskUrl(convId, id)`. Drives the mask-overlay in MessageView. */
  has_mask?: boolean;
  status?: "done" | "pending" | "error";
};

export const imageUrl = (convId: string, msgId: number, idx: number) =>
  `/api/conversations/${convId}/messages/${msgId}/image/${idx}`;

export const maskUrl = (convId: string, msgId: number) =>
  `/api/conversations/${convId}/messages/${msgId}/mask`;

export type ModelCapabilities = {
  vision: boolean;
  tools: boolean;
  chat: boolean;
  capabilities: string[];
  families: string[];
};

export type Me = { sub: string; username: string };

export type Status = {
  upstream: boolean;
  model_locked: boolean;
  auth: "dev" | "oidc" | "none";
  oidc_configured: boolean;
  oidc_ready: boolean;
  refiner_available: boolean;
  img2img_available: boolean;
  voice_in_available: boolean;
  voice_out_available: boolean;
  rag_available: boolean;
  web_search_available: boolean;
};

export type Document = {
  id: number;
  name: string;
  mime: string;
  size_bytes: number;
  chunk_count: number;
  created_at: number;
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
  embeddingModels: () =>
    fetch("/api/embedding-models", { credentials: "include" }).then(
      json<{ models: string[] }>,
    ),
  listDocuments: () =>
    fetch("/api/documents", { credentials: "include" }).then(json<Document[]>),
  uploadDocument: (body: {
    name: string;
    content_b64: string;
    mime?: string;
    model?: string;
  }) =>
    fetch("/api/documents", {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then(json<Document>),
  deleteDocument: (id: number) =>
    fetch(`/api/documents/${id}`, {
      method: "DELETE",
      credentials: "include",
    }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
};

export type ChatStreamEvent =
  | { type: "delta"; content: string }
  | { type: "done"; conv_id: string }
  | { type: "error"; message: string }
  | { type: "progress"; value: number; max: number }
  | { type: "preview"; mime: string; b64: string }
  /** Image-mode only: the job is waiting. Either on a slot in this
   * backend's image-gen semaphore (no `ahead`), or behind other jobs in
   * the shared ComfyUI host's own queue (`ahead` = jobs in front of ours,
   * which may belong to another backend). Re-sent whenever `ahead`
   * changes. */
  | { type: "queued"; ahead?: number }
  /** Text-mode only: end-of-turn generation stats. Lives only for the
   * current SSE stream — not persisted yet, so reload drops them. */
  | {
      type: "stats";
      tokens: number;
      prompt_tokens: number;
      tokens_per_sec: number;
    }
  /** Text-mode only: retrieval consulted the listed sources — uploaded
   * documents, live web results, or both — and injected them as system
   * context. Fired once per turn before the first delta. */
  | { type: "context"; sources: Source[] };

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
    /** When set, the backend trims everything strictly after this user
     * turn and regenerates the assistant reply off it. Used by the
     * regenerate button under user bubbles. */
    regenerate_from_user?: number;
    /** Image-mode sub-routing. "inpaint" requires `mask` + exactly one
     * image; "img2img" forces the Kontext branch; "txt2img" forces the
     * Ollama branch. Omit to let the backend infer from `images` /
     * `mask` (preserves the legacy behaviour). */
    sub_mode?: "txt2img" | "img2img" | "inpaint";
    /** Base64 PNG mask aligned to `images[0]`. White pixels (red
     * channel) mark the area to repaint. Only meaningful with
     * `sub_mode: "inpaint"`. */
    mask?: string;
    /** Negative-prompt override for image mode. Effective on workflows
     * that run real CFG (Flux Fill inpaint today). */
    negative?: string;
    /** Run a live web search before answering and inject the results as
     * context. A modifier on chat mode — ignored for image turns and on
     * deploys with no search provider configured. */
    web_search?: boolean;
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
  if (event === "queued") {
    try {
      const parsed = JSON.parse(data) as { ahead?: number };
      return {
        type: "queued",
        ahead: typeof parsed.ahead === "number" ? parsed.ahead : undefined,
      };
    } catch {
      return { type: "queued" };
    }
  }
  if (event === "context") {
    try {
      const parsed = JSON.parse(data) as { sources: Source[] };
      return { type: "context", sources: parsed.sources ?? [] };
    } catch {
      return null;
    }
  }
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
