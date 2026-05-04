import { useTheme } from "@emotion/react";
import {
  KeyboardEvent,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import ModelPicker from "./ModelPicker";

type Props = {
  onSend: (content: string, images?: string[]) => void;
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
};

/**
 * Single-container composer: textarea on top, action row below, all wrapped
 * in one rounded outline. Following the design reference (Pulp-Fiction-flavor
 * "royale with chat") — flat surface, soft border, 16px radius, accent send
 * button. No bottom hint line; the rounded shell carries the whole input.
 */
const Composer = ({
  onSend,
  streaming,
  onStop,
  history = [],
  model,
  onModelChange,
  vision,
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
    onSend(trimmed, imgs);
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
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result));
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      // Strip the `data:image/png;base64,` prefix — Ollama wants raw base64.
      const comma = dataUrl.indexOf(",");
      const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
      next.push({ base64, preview: dataUrl });
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
          borderRadius: 18,
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
            {vision && (
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
                  css={{
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
                  }}
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

export default Composer;
