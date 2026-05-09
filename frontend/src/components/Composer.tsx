import { Theme, useTheme } from "@emotion/react";
import {
  KeyboardEvent,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import { resizeImageForUpload } from "../image";
import ModelPicker from "./ModelPicker";

type Mode = "chat" | "image";

type Persona = {
  id: string;
  label: string;
  description: string;
};

type Props = {
  onSend: (
    content: string,
    images?: string[],
    mode?: Mode,
    refine?: boolean,
    persona?: string,
  ) => void;
  /** When true, the send button switches to a stop button. */
  streaming?: boolean;
  /** Called when the user clicks the stop button mid-stream. */
  onStop?: () => void;
  /**
   * Past user message contents, **newest first**. Used by ArrowUp/ArrowDown
   * shell-style recall: pressing up at the top of the textarea pulls in the
   * previous user message; pressing down at the bottom walks forward and
   * eventually restores the live draft.
   */
  history?: string[];
  /** Currently selected model name. */
  model?: string | null;
  /** Called when the user picks a different model. */
  onModelChange?: (next: string) => void;
  /** When true, the + image-attach button is shown for the current model. */
  vision?: boolean;
  /** Whether the model supports plain text chat output. */
  chatCap?: boolean;
  /** Whether the model supports image generation output. */
  imageGen?: boolean;
  /** Whether the server has a prompt refiner model configured. */
  refinerAvailable?: boolean;
  /** Available image-prompt personas. */
  personas?: Persona[];
};

/**
 * Single-container composer: textarea on top, action row below, all wrapped
 * in one rounded outline. Following the design reference (Pulp-Fiction-flavor
 * "royale with chat") — flat surface, soft border, 16px radius, accent send
 * button. No bottom hint line; the rounded shell carries the whole input.
 */
const REFINE_KEY = "chat:refineImagePrompt";
const PERSONA_KEY = "chat:imagePersona";

const Composer = ({
  onSend,
  streaming,
  onStop,
  history = [],
  model,
  onModelChange,
  vision,
  chatCap = true,
  imageGen = false,
  refinerAvailable = false,
  personas = [],
}: Props) => {
  const theme = useTheme();
  const [value, setValue] = useState("");
  // -1 = live draft, 0 = newest user message, 1 = previous, ...
  const [recallIndex, setRecallIndex] = useState(-1);
  const draftRef = useRef("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [focused, setFocused] = useState(false);
  // Pending image attachments for the next send. Each entry is the raw
  // base64 (no `data:` prefix) that Ollama expects.
  const [attached, setAttached] = useState<
    { base64: string; preview: string }[]
  >([]);
  const [dragOver, setDragOver] = useState(false);
  const [mode, setMode] = useState<Mode>(
    imageGen && !chatCap ? "image" : "chat",
  );
  // Reset mode when model caps change. Image-only model → image; otherwise
  // default to chat (user can still toggle if both are supported).
  const [lastCapsKey, setLastCapsKey] = useState(`${chatCap}|${imageGen}`);
  const capsKey = `${chatCap}|${imageGen}`;
  if (lastCapsKey !== capsKey) {
    setLastCapsKey(capsKey);
    setMode(imageGen && !chatCap ? "image" : "chat");
  }
  const showModeToggle = chatCap && imageGen;
  const [refine, setRefine] = useState<boolean>(() => {
    try {
      const v = window.localStorage.getItem(REFINE_KEY);
      return v == null ? true : v === "1";
    } catch {
      return true;
    }
  });
  const showRefineToggle = mode === "image" && refinerAvailable;
  const toggleRefine = () => {
    setRefine((prev) => {
      const next = !prev;
      try {
        window.localStorage.setItem(REFINE_KEY, next ? "1" : "0");
      } catch {
        // ignore
      }
      return next;
    });
  };
  const [persona, setPersona] = useState<string>(() => {
    try {
      return window.localStorage.getItem(PERSONA_KEY) ?? "default";
    } catch {
      return "default";
    }
  });
  const showPersonaPicker =
    mode === "image" && refinerAvailable && refine && personas.length > 0;
  const onPersonaChange = (next: string) => {
    setPersona(next);
    try {
      window.localStorage.setItem(PERSONA_KEY, next);
    } catch {
      // ignore
    }
  };
  // Counter ref so nested children's dragenter/leave events don't flicker
  // the highlight; the overlay only goes away once drag has fully left
  // the shell.
  const dragDepthRef = useRef(0);

  useLayoutEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const max = Math.floor(window.innerHeight * 0.55);
    el.style.height = `${Math.min(el.scrollHeight, max)}px`;
  }, [value]);

  useEffect(() => {
    const onResize = () => {
      const el = textareaRef.current;
      if (!el) return;
      el.style.height = "auto";
      const max = Math.floor(window.innerHeight * 0.55);
      el.style.height = `${Math.min(el.scrollHeight, max)}px`;
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const submit = () => {
    const trimmed = value.trim();
    if ((!trimmed && attached.length === 0) || streaming) return;
    const imgs =
      attached.length > 0 ? attached.map((a) => a.base64) : undefined;
    onSend(
      trimmed,
      imgs,
      mode,
      mode === "image" ? refine : undefined,
      mode === "image" && refine ? persona : undefined,
    );
    setValue("");
    setAttached([]);
    if (recallIndex !== -1) setRecallIndex(-1);
    draftRef.current = "";
  };

  const onPickFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    const next = [...attached];
    for (const file of Array.from(files)) {
      if (!file.type.startsWith("image/")) continue;
      try {
        const resized = await resizeImageForUpload(file);
        next.push(resized);
      } catch (err) {
        console.error("image resize failed", err);
      }
    }
    setAttached(next);
  };

  const removeAttachment = (i: number) => {
    setAttached((prev) => prev.filter((_, idx) => idx !== i));
  };

  const onDragEnter = (e: React.DragEvent<HTMLDivElement>) => {
    if (!vision) return;
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    dragDepthRef.current++;
    setDragOver(true);
  };
  const onDragLeave = (e: React.DragEvent<HTMLDivElement>) => {
    if (!vision) return;
    e.preventDefault();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (dragDepthRef.current === 0) setDragOver(false);
  };
  const onDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!vision) return;
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };
  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    if (!vision) return;
    e.preventDefault();
    dragDepthRef.current = 0;
    setDragOver(false);
    void onPickFiles(e.dataTransfer.files);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
      return;
    }

    if (e.nativeEvent.isComposing || history.length === 0) return;

    const el = e.currentTarget;
    const { selectionStart, selectionEnd } = el;

    if (e.key === "ArrowUp") {
      const inFirstLine = !value.slice(0, selectionStart).includes("\n");
      const inRecall = recallIndex !== -1;
      if (!inFirstLine && !inRecall) return;
      if (recallIndex >= history.length - 1) return;

      e.preventDefault();
      if (recallIndex === -1) draftRef.current = value;
      const next = recallIndex + 1;
      setRecallIndex(next);
      setValue(history[next]);
      requestAnimationFrame(() => {
        if (textareaRef.current) {
          const end = textareaRef.current.value.length;
          textareaRef.current.setSelectionRange(end, end);
        }
      });
      return;
    }

    if (e.key === "ArrowDown" && recallIndex !== -1) {
      const inLastLine = !value.slice(selectionEnd).includes("\n");
      if (!inLastLine) return;

      e.preventDefault();
      const next = recallIndex - 1;
      setRecallIndex(next);
      setValue(next === -1 ? draftRef.current : history[next]);
      if (next === -1) draftRef.current = "";
      return;
    }
  };

  const onChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setValue(e.target.value);
    if (recallIndex !== -1) {
      setRecallIndex(-1);
      draftRef.current = "";
    }
  };

  const canSend = (!!value.trim() || attached.length > 0) && !streaming;

  return (
    <div css={{}}>
      <div
        onDragEnter={onDragEnter}
        onDragLeave={onDragLeave}
        onDragOver={onDragOver}
        onDrop={onDrop}
        css={{
          position: "relative",
          maxWidth: 790,
          margin: "0 auto",
          border: `1px solid ${
            dragOver
              ? theme.colors.activity.on
              : focused
                ? theme.colors.text.muted
                : theme.colors.border
          }`,
          borderRadius: 16,
          background: theme.colors.background.light,
          padding: "10px 12px",
          transition: "border-color 120ms ease, background 120ms ease",
          display: "flex",
          flexDirection: "column",
          gap: 6,
          ...(dragOver ? { background: theme.colors.activity.onSoft } : {}),
        }}
      >
        {dragOver && (
          <div
            css={{
              position: "absolute",
              inset: 0,
              borderRadius: 18,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              pointerEvents: "none",
              ...theme.typography.body2,
              color: theme.colors.text.main,
              fontWeight: 500,
            }}
          >
            <span
              className="material-icons-outlined"
              css={{
                fontSize: 18,
                marginRight: 6,
                color: theme.colors.activity.on,
              }}
            >
              add_photo_alternate
            </span>
            drop image to attach
          </div>
        )}
        {attached.length > 0 && (
          <div
            css={{
              display: "flex",
              gap: 8,
              flexWrap: "wrap",
              padding: "4px 4px 0",
            }}
          >
            {attached.map((a, i) => (
              <div
                key={a.preview}
                css={{
                  position: "relative",
                  width: 56,
                  height: 56,
                  borderRadius: 8,
                  overflow: "hidden",
                  border: `1px solid ${theme.colors.border}`,
                  background: theme.colors.background.main,
                }}
              >
                <img
                  src={a.preview}
                  alt=""
                  css={{ width: "100%", height: "100%", objectFit: "cover" }}
                />
                <button
                  type="button"
                  aria-label="remove attachment"
                  onClick={() => removeAttachment(i)}
                  css={{
                    position: "absolute",
                    top: 2,
                    right: 2,
                    width: 18,
                    height: 18,
                    borderRadius: "50%",
                    border: "none",
                    background: "rgba(0,0,0,0.55)",
                    color: "#fff",
                    cursor: "pointer",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    padding: 0,
                  }}
                >
                  <span
                    className="material-icons-outlined"
                    css={{ fontSize: 12 }}
                  >
                    close
                  </span>
                </button>
              </div>
            ))}
          </div>
        )}
        <textarea
          ref={textareaRef}
          value={value}
          onChange={onChange}
          onKeyDown={onKeyDown}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          placeholder="message"
          rows={1}
          css={{
            border: "none",
            outline: "none",
            background: "transparent",
            resize: "none",
            width: "100%",
            padding: "4px 4px 0",
            ...theme.typography.body1,
            color: theme.colors.text.main,
            lineHeight: 1.5,
            minHeight: 28,
            maxHeight: "55vh",
            "&::placeholder": { color: theme.colors.text.muted },
          }}
        />
        <div
          css={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 12,
            paddingTop: 2,
          }}
        >
          <div css={{ display: "flex", alignItems: "center", gap: 4 }}>
            {vision && mode === "chat" && (
              <>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept="image/*"
                  multiple
                  onChange={(e) => {
                    void onPickFiles(e.target.files);
                    e.target.value = "";
                  }}
                  css={{ display: "none" }}
                />
                <button
                  type="button"
                  aria-label="attach image"
                  onClick={() => fileInputRef.current?.click()}
                  css={composerSubButtonCss(theme)}
                >
                  <span
                    className="material-icons-outlined"
                    css={{ fontSize: 22 }}
                  >
                    add
                  </span>
                </button>
              </>
            )}
            {showModeToggle && (
              <button
                type="button"
                aria-label={
                  mode === "chat"
                    ? "switch to image generation"
                    : "switch to chat"
                }
                title={
                  mode === "chat"
                    ? "switch to image generation"
                    : "switch to chat"
                }
                onClick={() => setMode(mode === "chat" ? "image" : "chat")}
                css={composerSubButtonCss(theme)}
              >
                <span
                  className="material-icons-outlined"
                  css={{ fontSize: 22 }}
                >
                  {mode === "chat" ? "image" : "chat_bubble_outline"}
                </span>
              </button>
            )}
            {showRefineToggle && (
              <RefineControl
                refine={refine}
                onToggleRefine={toggleRefine}
                personas={showPersonaPicker ? personas : []}
                persona={persona}
                onPersonaChange={onPersonaChange}
              />
            )}
          </div>
          <div css={{ display: "flex", alignItems: "center", gap: 12 }}>
            {onModelChange && (
              <ModelPicker value={model ?? null} onChange={onModelChange} />
            )}
            {streaming && onStop ? (
              <button
                type="button"
                onClick={onStop}
                aria-label="stop generating"
                css={iconButtonCss(theme.colors.text.main, "#fff")}
              >
                <span
                  className="material-icons-outlined"
                  css={{ fontSize: 20 }}
                >
                  stop
                </span>
              </button>
            ) : (
              <button
                type="button"
                onClick={submit}
                disabled={!canSend}
                aria-label="send"
                css={{
                  ...iconButtonCss(theme.colors.activity.on, "#fff"),
                  "&:disabled": {
                    opacity: 0.35,
                    cursor: "not-allowed",
                  },
                }}
              >
                <span
                  className="material-icons-outlined"
                  css={{ fontSize: 20 }}
                >
                  arrow_upward
                </span>
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};

const RefineControl = ({
  refine,
  onToggleRefine,
  personas,
  persona,
  onPersonaChange,
}: {
  refine: boolean;
  onToggleRefine: () => void;
  personas: Persona[];
  persona: string;
  onPersonaChange: (id: string) => void;
}) => {
  const theme = useTheme();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const showChevron = personas.length > 0;
  const accent = refine ? theme.colors.activity.on : theme.colors.text.muted;

  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      const el = wrapRef.current;
      if (!el || !(e.target instanceof Node) || !el.contains(e.target)) {
        setOpen(false);
      }
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("pointerdown", onPointer);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onPointer);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div
      ref={wrapRef}
      className="refine-control"
      css={{
        position: "relative",
        display: "inline-flex",
        alignItems: "center",
        gap: 0,
        ...(showChevron && {
          "& .refine-chevron": {
            opacity: 0,
            width: 0,
            transition: "opacity 120ms ease, width 120ms ease",
            pointerEvents: "none",
          },
          "&:hover .refine-chevron, &:focus-within .refine-chevron": {
            opacity: 1,
            width: 18,
            pointerEvents: "auto",
          },
          "@media (hover: none)": {
            "& .refine-chevron": {
              opacity: 1,
              width: 18,
              pointerEvents: "auto",
            },
          },
        }),
        ...(open && {
          "& .refine-chevron": {
            opacity: 1,
            width: 18,
            pointerEvents: "auto",
          },
        }),
      }}
    >
      <button
        type="button"
        aria-label={refine ? "refine prompt: on" : "refine prompt: off"}
        title={
          refine
            ? "prompt refiner is on — model expands the prompt before generation"
            : "prompt refiner is off — model describes the result for next-turn context"
        }
        aria-pressed={refine}
        onClick={onToggleRefine}
        css={{
          ...composerSubButtonCss(theme),
          color: accent,
        }}
      >
        <span className="material-icons-outlined" css={{ fontSize: 22 }}>
          auto_fix_high
        </span>
      </button>
      {showChevron && (
        <button
          type="button"
          className="refine-chevron"
          aria-label="choose prompt persona"
          aria-haspopup="menu"
          aria-expanded={open}
          title={
            personas.find((p) => p.id === persona)?.label ?? "prompt persona"
          }
          onClick={() => setOpen((v) => !v)}
          css={{
            height: 28,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            border: "none",
            background: "transparent",
            color: accent,
            cursor: "pointer",
            padding: 0,
            overflow: "hidden",
            "&:hover": { color: theme.colors.text.main },
          }}
        >
          <span className="material-icons-outlined" css={{ fontSize: 18 }}>
            {open ? "expand_less" : "expand_more"}
          </span>
        </button>
      )}
      {open && (
        <div
          role="menu"
          css={{
            position: "absolute",
            bottom: "calc(100% + 8px)",
            left: 0,
            minWidth: 240,
            maxWidth: 320,
            padding: 6,
            borderRadius: 12,
            border: `1px solid ${theme.colors.border}`,
            background: theme.colors.background.main,
            boxShadow: theme.shadows.main,
            display: "flex",
            flexDirection: "column",
            gap: 2,
            zIndex: 20,
          }}
        >
          {personas.map((p) => {
            const selected = p.id === persona;
            return (
              <button
                key={p.id}
                type="button"
                role="menuitemradio"
                aria-checked={selected}
                onClick={() => {
                  onPersonaChange(p.id);
                  setOpen(false);
                }}
                css={{
                  textAlign: "left",
                  border: "none",
                  borderRadius: 8,
                  padding: "8px 10px",
                  background: selected
                    ? theme.colors.activity.onSoft
                    : "transparent",
                  color: theme.colors.text.main,
                  cursor: "pointer",
                  display: "flex",
                  flexDirection: "column",
                  gap: 2,
                  "&:hover": {
                    background: selected
                      ? theme.colors.activity.onSoft
                      : theme.colors.background.light,
                  },
                }}
              >
                <span
                  css={{
                    ...theme.typography.body2,
                    fontWeight: selected ? 600 : 500,
                  }}
                >
                  {p.label}
                </span>
                <span
                  css={{
                    ...theme.typography.caption,
                    color: theme.colors.text.muted,
                  }}
                >
                  {p.description}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
};

const iconButtonCss = (bg: string, fg: string) => ({
  width: 36,
  height: 36,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  borderRadius: 10,
  border: "none",
  background: bg,
  color: fg,
  cursor: "pointer",
});

const composerSubButtonCss = (theme: Theme) => ({
  width: 32,
  height: 32,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  border: "none",
  borderRadius: 8,
  background: "transparent",
  color: theme.colors.text.muted,
  cursor: "pointer",
  "&:hover": {
    background: theme.colors.background.main,
    color: theme.colors.text.main,
  },
});

export default Composer;
