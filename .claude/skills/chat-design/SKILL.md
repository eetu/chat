---
name: chat-design
description: Visual identity for chat (full wordmark "royale with chat.") — a sibling in eetu's homebrew web app family. Layers chat's glyph, wordmark, layout, and voice on top of the shared halo-design tokens. Use when building or styling chat's UI, or generating branded assets/mocks.
user-invocable: true
---

# chat-design

Shared tokens + conventions come from `halo-design` — copy `colors_and_type.css`
verbatim. In this repo the tokens live, mirrored, in `frontend/src/themes.ts`;
read them via `useTheme()` and style with Emotion's `css` prop — never hardcode
hex. Below is chat's delta.

## The four deltas

**Glyph** — a **rounded chat bubble + warm centre dot**. 64×64, `currentColor`
outline with a small tail bottom-left so it reads as a bubble at favicon size,
the one hardcoded color a warm `#f78f08` dot (the family "warm centre"). Stroke
~3, round joins — same weight as halo's ring. Source:
`frontend/public/favicon.svg` (rasters `icon-{192,512}.png` /
`apple-touch-icon.png`; regen recipe in `frontend/public/README.md`).

**Wordmark** — `royale with chat` + accent period (Pulp Fiction riff on "Royale
with Cheese"). Inter 600, lowercase, `-0.04em`. Collapses to bare `chat.` under
600px (the `royale with ` prefix drops), same way halo collapses to its glyph.
Alternate short form `le chat.` — use sparingly. Source:
`frontend/src/components/Wordmark.tsx`.

**Layout / density** — **two columns, no chrome**: sidebar (wordmark → new-chat
→ conversation list, swipe-left on touch to delete) + thread (centered, max-width
760px; user input is a right-aligned tinted bubble, assistant output is rendered
full-width as a markdown document, no bubble). Composer is one rounded shell
pinned to the bottom. No top nav, no settings UI — settings are server env vars.
Sparse vs halo's data-density. Single 600px breakpoint (`src/mq.ts`).

**Voice** — lowercase, terse, mildly pulpy. Numbers and conversations do the
talking. No marketing tone, no exclamation marks, no emoji; Material Icons
Outlined for any in-UI glyph that isn't the brand mark. Empty/error states get
one quotable line each:
- empty landing: *"the path of the righteous prompt is beset on all sides."*
- login gate: *"you brought a knife to a gunfight. sign in first."*
- 404: *"english, motherfucker, do you speak it?"*

## Differences from halo / ocular / scribe

| | chat |
|---|---|
| Stack | Rust (actix-web) SSE proxy → Ollama + React 19 SPA (Emotion, TanStack Router) |
| Glyph | chat bubble + warm centre dot |
| Hero element | the streaming assistant message — token-by-token is the only motion |
| Accent use | send button, active-conversation border, user-bubble tint, typing dots |

## Source-of-truth files

- `frontend/src/themes.ts` — canonical tokens (verbatim mirror of halo).
- `frontend/src/components/Wordmark.tsx` — brand.
- `frontend/src/components/{Sidebar,Composer,MessageView,Markdown}.tsx` — the screen.
- `frontend/public/favicon.svg` — the glyph.
- `frontend/docs/renderer-extensions.md` — deferred renderer work (mermaid, RAG,
  fence/KaTeX balancing, code-copy button, …).
