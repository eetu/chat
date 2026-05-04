---
name: chat-design
description: Use this skill to generate well-branded interfaces and assets for the chat app (a self-hosted chat UI that talks to an Ollama LAN endpoint), either for production or throwaway prototypes/mocks. Contains essential design guidelines, colors, type, fonts, assets, and UI kit components for prototyping. Sibling product to halo — same design language, different glyph.
user-invocable: true
---

Read `README.md` in this skill, plus `colors_and_type.css` and `assets/`.

For production code, the source of truth lives in the host repo:
- Components: `frontend/src/components/`
- Theme tokens: `frontend/src/themes.ts` (copied verbatim from halo)
- Routes: `frontend/src/routes/` (TanStack Router, file-based)
- Wordmark: `frontend/src/components/Wordmark.tsx`

If creating visual artifacts (slides, mocks, throwaway prototypes, etc), copy
assets out and create static HTML files for the user to view. If working on
production code, refer to existing components first; do not recreate them as
JSX prototypes.

If the user invokes this skill without any other guidance, ask them what they
want to build or design, ask some questions, and act as an expert designer
who outputs HTML artifacts _or_ production code, depending on the need.

Key things to keep in mind for chat:

- **Sibling of halo.** Identical color palette, fonts, shadow, radius. Anyone
  who has used halo should immediately recognize the family. The only visual
  divergence is the wordmark glyph.
- **Wordmark is "royale with chat."** Pulp Fiction reference (Royale with
  Cheese). Inter 600, lowercase, `letter-spacing: -0.04em`. The word `chat`
  is followed by an orange accent period. On narrow screens the "royale with"
  prefix collapses, leaving just `chat.` — same way halo collapses to its
  glyph. Alternate short form: `le chat.` (use sparingly; default to
  "royale with chat").
- **Glyph: rounded chat bubble + warm centre dot.** Same `currentColor` thin
  stroke (3px) as halo's ring, same accent dot (`#f78f08`) inside. The bubble
  has a small tail bottom-left so it reads as a chat bubble at favicon size.
- **Voice.** Lowercase, terse, mildly pulpy. Numbers and conversations do
  the talking. Empty states and errors are allowed one quotable line each
  ("you brought a knife to a gunfight."). No marketing voice. No exclamation
  marks. No emoji.
- **Two columns, no chrome.** Sidebar (conversations list + new-chat button)
  on the left, thread + composer on the right. No top nav bar, no breadcrumbs,
  no settings cog. Settings live in env vars on the server, not the UI.
- **Streaming first.** The assistant bubble appears the instant a user sends,
  and content fills in token-by-token. Never block the UI on the full
  response — a half-rendered bubble is the default.
- **Cards: 6px radius, soft shadow** in light theme; shadow off in dark.
  Same as halo.
- **No emoji, no hero imagery.** Material Icons Outlined for any glyph that
  isn't the brand mark.
- **Touch-friendly.** Swipe-left on a conversation row reveals a delete
  action. Tap targets are large. No hover-only affordances.

## Differences from halo

| Aspect | halo | chat |
|---|---|---|
| Wordmark glyph | thin ring + warm centre | chat bubble + warm centre |
| Wordmark text | `halo.` | `royale with chat.` (collapses to `chat.`) |
| Layout | fixed 720px column with nav rail | full-width sidebar + thread |
| Locale | Finnish, lowercase | English, lowercase, Pulp Fiction flavor |
| Density | data-dense (clock, charts, cards) | sparse (one column of bubbles) |
| Motion | drawer unfold, stroke-dashoffset, breathing bulbs | none (yet) — stream is the motion |

Everything else — colors, fonts, shadow, radius, accent — is identical. Copy
forward from halo's `themes.ts` whenever it changes.
