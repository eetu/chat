# Renderer extensions (deferred)

Markdown/renderer features deliberately left out of the current `<Markdown>`
build. Wire any of these when the use-case actually shows up — don't add
speculatively. Each entry has its install command, the plugin/component
snippet, and the tradeoff that justified deferring it.

## Mermaid diagrams

```sh
yarn add mermaid
```

Custom code-fence component that intercepts `language-mermaid`:

```tsx
// in Markdown.tsx components map
code: ({ className, children, ...rest }) => {
  if (className === "language-mermaid") {
    return <Mermaid chart={String(children)} />;
  }
  return <code className={className} {...rest}>{children}</code>;
},
```

`<Mermaid>` calls `mermaid.render(id, chart)` in a `useEffect` and injects
the SVG via `dangerouslySetInnerHTML` (mermaid's own output, trusted).
Initialize with `mermaid.initialize({ startOnLoad: false, theme:
isDark ? "dark" : "default" })`.

Tradeoff: mermaid bundles ~500kb gzipped. Use dynamic `import("mermaid")` so
it only loads on conversations that actually contain a mermaid fence.

## Heading anchor slugs

```sh
yarn add rehype-slug rehype-autolink-headings
```

Add to `rehypePlugins` after `rehypeKatex`:

```ts
[rehypeSlug],
[rehypeAutolinkHeadings, { behavior: "wrap" }],
```

Useful only when assistant emits ToC links (`[Section](#section)`) — rare in
chat. Skip until needed.

## Emoji shortcodes

```sh
yarn add remark-emoji
```

Add to `remarkPlugins` before `remarkGfm`. Converts `:smile:` → 😀. LLMs
already emit unicode emoji directly, so this only helps if a model habitually
outputs shortcodes.

## Code copy button

No new dependency. Custom `pre` component override:

```tsx
pre: ({ children }) => {
  const codeText = useMemo(() => extractText(children), [children]);
  return (
    <div css={{ position: "relative" }}>
      <pre>{children}</pre>
      <CopyButton text={codeText} />
    </div>
  );
},
```

`<CopyButton>` calls `navigator.clipboard.writeText(text)` and swaps the
`content_copy` icon for `check` for 1.5s. Position absolute top-right
inside the relative wrapper, fade in on hover (touch: always visible at
~40% opacity).

## Streaming-aware fence balancing

During streaming, an open ```` ``` ```` fence renders the rest of the
message as a single ugly code block until the closing fence arrives.
Mitigation: pre-process the streaming content before passing it to
`<ReactMarkdown>`:

```ts
function balanceFences(md: string): string {
  const fenceCount = (md.match(/```/g) ?? []).length;
  return fenceCount % 2 === 1 ? md + "\n```" : md;
}
```

Pass `balanceFences(content)` from `MessageView` while `streaming`. Once
`done` fires, drop the wrapper — the persisted text is balanced server-side.

Tradeoff: minor; flickers a half-rendered block while streaming. Accept the
flicker until users complain.

## KaTeX dollar-sign streaming

Half-streamed `$...` looks ugly during streaming the same way as fences. The
same `balance` pattern works for `$$` blocks (count `$$` occurrences, append
`$$` if odd). Skip `$` inline math balancing — too many false positives
(prices, regex anchors, etc.).

## File ingestion / RAG (PDFs, docx, txt)

Ollama itself only accepts text + images. For document chat the
established pattern is **client-side or backend-side text extraction**,
not native upload. There is no Ollama "files" API.

Two scope tiers worth considering:

**Tier 1 — naive text injection.** Backend accepts a file upload, runs an
extractor (`pdf-extract` or `lopdf` for PDFs, `docx-rs` for docx, plain
read for txt/md), and pastes the text as a `system` or prefixed `user`
message in the next turn. Simple, zero infra, breaks for files larger
than the model's context window.

**Tier 2 — chunk + embed + retrieve (RAG).**
1. Persist files in a new SQLite table `attachments(id, conv_id, name,
   mime, bytes_path)`. Optionally external storage when bytes get big.
2. Extract → chunk (500-1000 tokens, ~100 token overlap).
3. Embed via Ollama's `/api/embed` (or `/api/embeddings` on older
   versions). Store vectors in a `chunks(id, attachment_id, ord, text,
   embedding BLOB)` table. SQLite + the `sqlite-vec` extension is the
   lightest option; otherwise spin up a dedicated vector store (qdrant,
   chroma) but that pulls in service complexity.
4. On every chat turn: embed the user query, top-k cosine over the
   conversation's chunks, prepend the chunks to the prompt as a
   `system` message ("relevant excerpts:\n\n…").
5. Frontend: same `+` button, accepts `application/pdf`, `text/plain`,
   `.md`, `.docx`. Show file chips above the textarea (similar to image
   thumbnails). On send, post the file via a separate endpoint first,
   get back an `attachment_id`, include it in the chat body. The
   backend handles all extraction async.

Tradeoffs: Tier 1 is half a day of work; Tier 2 is a week and an
embedding model dependency. Pick based on whether you actually want
multi-file conversations or just "here, look at this one PDF". Don't
build the embedding pipeline speculatively — Tier 1 covers most cases
when files are small.

## remark-directive for callouts

```sh
yarn add remark-directive
```

Enables `:::note`, `:::warning` block syntax for richer asides. Custom
`components` map renders each directive type with its own surface +
icon. Only useful if the assistant is prompted to emit them; otherwise
overhead.
