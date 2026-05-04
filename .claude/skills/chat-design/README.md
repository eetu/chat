# Handoff: chat — royale with chat

## Overview

**chat** (full wordmark: _royale with chat._) is a self-hosted chat UI for an
Ollama endpoint running on the LAN. It is the sibling product of
[halo](../../../../hcc) and ships with the same design tokens, fonts, and
warm orange accent — only the wordmark glyph and the layout density differ.

- Streaming token-by-token responses (SSE)
- Per-user conversation history (kanidm OIDC; SQLite backed)
- Configurable retention (default 30 days)
- New chat / list with swipe-to-delete
- No top navbar, no settings panel — just sidebar + thread

It's designed to be used from a phone, tablet, or laptop on the home network,
optionally over VPN. Touch-first; no hover-only affordances.

## Visual language

Identical to halo. See [`halo-design`](../../../../hcc/.claude/skills/halo_design/README.md)
for the full reference. Below is the chat-specific delta.

### Wordmark + glyph

- **Glyph.** 64×64 SVG. Rounded chat bubble with a 6px corner radius and a
  small tail at bottom-left. `stroke-width: 3`, `currentColor` stroke (so it
  inherits theme text color). A 5r filled circle accent (`#f78f08`) sits at
  approximately (32, 30) — visually centered above the tail.
  - Reference path:
    `M12 14h40a6 6 0 0 1 6 6v20a6 6 0 0 1-6 6H30l-9 9v-9h-9a6 6 0 0 1-6-6V20a6 6 0 0 1 6-6z`

- **Wordmark.** `royale with chat` in Inter 600 lowercase,
  `letter-spacing: -0.04em`, followed by an accent period (`color: #f78f08`).
  Below ~600px width, the `royale with ` prefix collapses, leaving `chat.`
  alone. Treat the wordmark as a single line — never wrap.

- **Sizing.** Default 22px (sidebar), 28–32px on full-screen states (login,
  empty landing). Maintain the 10px gap between glyph and wordmark.

### Layout

- **Two-column.** Sidebar 280px wide on desktop (≥600px), main thread
  fills remaining width. No top bar, no footer.
- **Sidebar can be hidden on every viewport.** A `chevron_left` button in
  the sidebar header collapses it. On desktop the sidebar is `display:
  none` (main expands to full width); on mobile it slides off-canvas. To
  reopen, tap the floating hamburger.
- **Floating hamburger.** When the sidebar is hidden, a 36×36 circular
  button appears `position: absolute` at top:12 / left:12 of the main
  pane. White (`background.main`) with 1px border + soft shadow + Material
  Icons Outlined `menu` glyph at 20px. Sits above content via `z-index:
  10`; thread content scrolls behind it (padding-top reserves the first
  60px so the initial frame is clear).
- **Mobile (≤600px) drawer.** Sidebar overlays at 82vw / max 320px with a
  semi-transparent scrim. Auto-closes on route change (TanStack Router
  `useLocation` effect) and forces back to defaults when crossing the
  breakpoint.
- **Breakpoint.** Single value, 600px (`mq[0]` from `src/mq.ts`). Use
  `useMediaQuery("(max-width: 600px)")` from `usehooks-ts` for component
  logic; use `mq[0]` for emotion css media queries — keep the two
  consistent.
- **Sidebar.** Wordmark top, then the "new chat" button, then the
  conversation list. Active conversation has a 2px accent left border + soft
  accent background (`theme.colors.activity.onSoft`).
- **Thread.** Centered max-width 760px. Bubbles align right (user) or left
  (assistant). Composer pinned to the bottom with a 1px top hairline.
- **Image attachments (vision models).** When the active model's `/api/show`
  capabilities include `"vision"` (or `details.families` includes `clip` /
  `mllama` / `siglip`), a 32×32 `add` icon button appears bottom-left of
  the composer. Click → native file picker (`accept="image/*"`,
  multiple). The composer shell also accepts **drag-and-drop**: dragging
  any file in flips the border to `activity.on`, fills the shell with a
  soft accent tint, and shows an `add_photo_alternate` + "drop image to
  attach" overlay. Drop → same path as picker. Drag handlers no-op on
  non-vision models so nothing visually changes. Picked files are read as base64 and shown as 56×56 rounded
  thumbnails above the textarea, each with a tiny `close` overlay to
  remove. On send, base64s ride along under `images: string[]` and are
  persisted to `messages.attachments` as a JSON array. User bubble
  renders the images as up to 220×220 rounded tiles above the text.
  Non-vision models hide the attach button entirely. Document
  ingestion (PDFs, docx) is **not** wired — see "File ingestion / RAG"
  in the Future Renderer Extensions section.
- **Composer.** Single rounded shell holds everything — textarea on top,
  one action row below, no separating borders inside. 18px radius, 1px
  outline that brightens on focus (`text.muted` instead of `border`).
  Background `background.light`. No keyboard hint copy below the shell.
  Placeholder reads `message`.
  - **Action row.** Right-aligned: model picker (borderless, muted
    text, OS chevron) then a 36×36 square button. Idle: orange
    `activity.on` background with white `arrow_upward` icon. Streaming:
    dark `text.main` background with `stop` icon (same square).
  - **Enter submits**; **Shift+Enter** newline. IME `isComposing`
    respected.
  - **Auto-grow textarea.** `rows={1}` + `useLayoutEffect` syncs
    `height` to `scrollHeight` on keystroke and resize, capped at
    `55vh` (a hair shorter than before so the action row stays visible
    on large drafts).
  - **History recall (`↑` / `↓`).** Shell-style. Pressing **ArrowUp**
    when the caret is in the first visual line of the textarea pulls
    in the most recent user message; further ArrowUps walk further
    back. Pressing **ArrowDown** in the last visual line walks forward
    and eventually restores the live draft (saved on first ArrowUp).
    Editing a recalled message commits it as a fresh draft —
    ArrowDown will not restore the old draft after that. Lets users
    stop a stream, hit ↑, tweak, send.
  - **Idle.** Accent-orange `send` pill button, disabled until the
    textarea has non-whitespace content.
  - **Streaming.** Replaced by a 44×44 dark square `stop` icon button
    (Material Icons Outlined `stop`, 22px). Click aborts the in-flight
    `fetch` via `AbortController`. The textarea stays editable so users
    can prepare the next prompt.
  - **Model picker.** Bottom-right of the composer footer row, native
    `<select>` styled to match the design. Hidden on the smallest
    breakpoint when keyboard hint collapses. When the server returns
    `{ name, locked: true }` (`OLLAMA_MODEL` env set), the picker
    collapses to a `lock` icon + read-only model label. Per-conversation
    state — the chosen model is persisted on the conversation row on
    first send and seeds the picker on reopen. Fresh chats fall back to
    `localStorage["chat:lastModel"]`.
- **Empty landing (`/`).** Wordmark, one quotable line, single CTA button
  (`new chat`). No illustrations.

### Messages

Asymmetric on purpose — user input is "speech", assistant output is "the
document being written together". Different containers reflect that.

- **User.** Right-aligned bubble. 6px radius, 10/14px padding. Background
  `theme.colors.activity.onSoft` (accent at 10–20% depending on theme). No
  border. Plain `white-space: pre-wrap` — never render user-supplied
  markdown.
- **Waiting state.** Before the first delta arrives the assistant slot
  renders `<TypingIndicator>` — three 7px orange (`activity.on`) dots,
  pulsing with the halo design vocabulary (1.2s ease-in-out, opacity
  0.25→1 + scale 0.85→1, stagger 0.18s). Same warm accent as the
  wordmark and lit-bulb glow; only saturated colour on screen.
- **Stick-to-bottom scroll.** While streaming, the thread auto-follows
  new content **only if the user is already within 60px of the
  bottom**. Scrolling up at any point breaks the lock and leaves the
  viewport alone — incoming deltas no longer yank the user back. A
  small circular `arrow_downward` button (36×36, surface bg + soft
  shadow + 1px border) appears 96px above the composer when the user
  scrolled away during a stream; clicking it re-glues and smooth-scrolls
  to the latest. Sending a new user message re-glues unconditionally
  (they expect to see what they just typed). On conversation switch the
  view force-scrolls to the latest.
- **Assistant.** No bubble. Full column width, no surface, no border, no
  inner padding. Sits directly on the body background like a document
  paragraph. Markdown rendered via `<Markdown>`:
  - **GFM** — tables, task lists, strikethrough, autolinks, footnotes
  - **Hard breaks** — single `\n` → `<br>` (`remark-breaks`), matching
    GitHub-comment behavior; LLMs commonly use single newlines for line
    wraps
  - **Code blocks** — highlight.js auto-detect, GitHub-light/dark palette
    toggled by `prefers-color-scheme`
  - **Math** — KaTeX, `$inline$` and `$$block$$`. Block math overflows
    horizontally on narrow columns
  - **Tables** — wrapped in horizontal-scroll container so wide tables
    don't break narrow viewports
  - **Links** — external (`http(s)://`) get `target="_blank"
    rel="noopener noreferrer"`; internal links open in-place
  - **Images** — `max-width: 100%`, `loading="lazy"`, 4px radius
  - **Raw HTML** — escaped, not rendered (no `rehype-raw`)
  Horizontal padding comes only from the page column gutter — do not
  re-introduce per-message inset padding.
- Code fences use a 1px border + faint surface fill; inline code uses a
  small rounded chip. Highlight palette switches with `prefers-color-scheme`.
- Empty assistant bubble during streaming shows a faint `…` placeholder.

### Delete affordances

Two paths, gated by input type — trackpad pans were being misread as swipes,
so swipe is touch-only.

- **Touch (`pointerType === "touch"`).** Drag the row left up to 200px.
  Release thresholds:
  - `≤ -200px` → commit delete
  - `-200 .. -88px` → snap open at -88px (full-height red `delete` panel
    revealed on the right, `theme.colors.error`, Space Grotesk 13, white)
  - `> -88px` → snap closed
- **Mouse / pen.** Hover the row → small `close` icon (Material Icons
  Outlined, 18px, muted) fades in at the row's right edge. Click → native
  `window.confirm` before deleting. The swipe gesture is disabled in this
  mode so two-finger trackpad pans don't trigger it.

### Voice

- Lowercase. Terse. Pulp Fiction flavor in copy. No exclamation marks.
- Allowed quotables (one per surface, max):
  - Empty landing: `the path of the righteous prompt is beset on all sides.`
  - Login gate: `you brought a knife to a gunfight. sign in first.`
  - 404 (when added): `english, motherfucker, do you speak it?`
- Buttons: imperative lowercase (`new chat`, `send`, `sign in`, `delete`).
- Time formatting: relative (`2m`, `5h`, `3d`). Absolute timestamps only in
  conversation metadata if needed.

### Motion

- **None yet.** The streaming response is the only motion in the app.
- If adding motion: match halo's vocabulary — drawer unfolds at 150ms ease,
  no springs, no scroll-linked animations, no ripples.

## Design tokens

All tokens are inherited from halo via `frontend/src/themes.ts` (copied
verbatim). See `colors_and_type.css` in this skill for a CSS-variable export.

The values that matter most for chat:

| Token | Use |
|---|---|
| `colors.activity.on` (`#f78f08`) | accent dot, send button, active conv border |
| `colors.activity.onSoft` (`rgba(247,143,8,0.10/0.20)`) | user bubble, active conv background |
| `colors.background.main` | assistant bubble, composer background |
| `colors.background.light` | sidebar background |
| `colors.text.main` | bubble text |
| `colors.text.muted` | timestamps, captions |
| `colors.border` | hairlines, assistant bubble border |
| `colors.error` | swipe-to-delete background |
| `border.radius` (6px) | every card and bubble |

## Future renderer extensions

Deliberately deferred from the current `<Markdown>` build. Wire any of these
when the use-case actually shows up — don't add speculatively.

### Mermaid diagrams

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

### Heading anchor slugs

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

### Emoji shortcodes

```sh
yarn add remark-emoji
```

Add to `remarkPlugins` before `remarkGfm`. Converts `:smile:` → 😀. LLMs
already emit unicode emoji directly, so this only helps if a model habitually
outputs shortcodes.

### Code copy button

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

### Streaming-aware fence balancing

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

### KaTeX dollar-sign streaming

Half-streamed `$...` looks ugly during streaming the same way as fences. The
same `balance` pattern works for `$$` blocks (count `$$` occurrences, append
`$$` if odd). Skip `$` inline math balancing — too many false positives
(prices, regex anchors, etc.).

### File ingestion / RAG (PDFs, docx, txt)

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

### remark-directive for callouts

```sh
yarn add remark-directive
```

Enables `:::note`, `:::warning` block syntax for richer asides. Custom
`components` map renders each directive type with its own surface +
icon. Only useful if the assistant is prompted to emit them; otherwise
overhead.

---

## Iconography

Material Icons Outlined for in-UI glyphs. So far the only in-UI icon is `add`
(new-chat button). Add icons sparingly.

## Files in this bundle

```
chat-design/
├── SKILL.md             skill manifest
├── README.md            this file (full design reference)
├── colors_and_type.css  CSS custom properties for prototyping
└── assets/
    ├── chat-logo.svg    glyph alone
    └── chat-wordmark.svg full lockup (glyph + wordmark)
```

To prototype: import `colors_and_type.css`, drop `chat-logo.svg` for the
brand mark. Inter and Space Grotesk are loaded from Google Fonts CDN —
self-host for offline work.

## Production target

The host repo (`chat/`) is Vite + React 19 + Emotion + TanStack Router
(file-based). Recreate prototypes in production using:

- Themed components via Emotion's `css` prop
- Material Icons Outlined via the Google Fonts CDN
- TanStack Router `createFileRoute("/path")` for any new screen
- SWR for cached fetches; `useChat` hook for streaming flows

Do not introduce a separate state library. Do not hand-edit `routeTree.gen.ts`.
