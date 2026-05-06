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
  images?: string[];
};

export type ModelCapabilities = {
  vision: boolean;
  tools: boolean;
  chat: boolean;
  image_gen: boolean;
  capabilities: string[];
  families: string[];
};

export type Me = { sub: string; username: string };

const json = async <T>(res: Response): Promise<T> => {
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`${res.status} ${text}`);
  }
  return res.json() as Promise<T>;
};

export const api = {
  me: () => fetch("/api/me", { credentials: "include" }).then(json<Me>),
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
  getMessages: (id: string) =>
    fetch(`/api/conversations/${id}/messages`, {
      credentials: "include",
    }).then(json<Message[]>),
  modelCaps: (model: string) =>
    fetch(`/api/models/caps?model=${encodeURIComponent(model)}`, {
      credentials: "include",
    }).then(json<ModelCapabilities>),
};

export type ChatStreamEvent =
  | { type: "delta"; content: string }
  | { type: "done"; conv_id: string }
  | { type: "error"; message: string };

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
  return null;
}
