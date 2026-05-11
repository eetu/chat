import { Theme, useTheme } from "@emotion/react";
import {
  KeyboardEvent,
  Ref,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import { resizeImageForUpload } from "../image";
import { mq } from "../mq";
import ModelPicker from "./ModelPicker";

/**
 * Imperative handle parents can grab to seed the composer with an
 * incoming image — used by the "remix" action on a generated image to
 * pre-fill the attachment row and flip the img2img toggle without going
 * through the file picker again.
 */
export type ComposerHandle = {
  remixWithImage: (image: { base64: string; preview: string }) => void;
};

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
  /** Whether the server has an img2img backend wired up (ComfyUI Kontext
   * today). Drives the inline indicator under the attachment row —
   * without it the backend silently falls back to Ollama txt2img. */
  img2imgAvailable?: boolean;
  /** Whether the server can transcribe audio (whisper.cpp endpoint
   * configured). Drives the mic affordance in the action row. */
  voiceInAvailable?: boolean;
  /** Available image-prompt personas. */
  personas?: Persona[];
  /** Imperative-handle ref. React 19 takes refs as plain props. */
  ref?: Ref<ComposerHandle>;
};

/**
 * Single-container composer: textarea on top, action row below, all wrapped
 * in one rounded outline. Following the design reference (Pulp-Fiction-flavor
 * "royale with chat") — flat surface, soft border, 16px radius, accent send
 * button. No bottom hint line; the rounded shell carries the whole input.
 */
const REFINE_KEY = "chat:refineImagePrompt";
const PERSONA_KEY = "chat:imagePersona";
const IMG2IMG_KEY = "chat:img2img";

/**
 * Decode an arbitrary MediaRecorder blob into 16 kHz mono 16-bit PCM
 * WAV. whisper.cpp's HTTP server uses dr_wav and only accepts WAV; opus
 * / webm / m4a all bounce with a 400. The Web Audio API handles the
 * decode + resample for us — `new AudioContext({ sampleRate: 16000 })`
 * forces resample on decodeAudioData, and downmixing to mono is a
 * straightforward channel average.
 */
async function encodeAsWav(blob: Blob): Promise<Blob> {
  const bytes = await blob.arrayBuffer();
  const AC =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext: typeof AudioContext })
      .webkitAudioContext;
  const ctx = new AC({ sampleRate: 16000 });
  let decoded: AudioBuffer;
  try {
    decoded = await ctx.decodeAudioData(bytes);
  } finally {
    void ctx.close().catch(() => {});
  }
  const channels = decoded.numberOfChannels;
  const length = decoded.length;
  const mono = new Float32Array(length);
  for (let c = 0; c < channels; c++) {
    const data = decoded.getChannelData(c);
    for (let i = 0; i < length; i++) mono[i] += data[i] / channels;
  }
  const buf = new ArrayBuffer(44 + length * 2);
  const view = new DataView(buf);
  let off = 0;
  const writeStr = (s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off++, s.charCodeAt(i));
  };
  writeStr("RIFF");
  view.setUint32(off, 36 + length * 2, true);
  off += 4;
  writeStr("WAVE");
  writeStr("fmt ");
  view.setUint32(off, 16, true);
  off += 4; // PCM chunk size
  view.setUint16(off, 1, true);
  off += 2; // format = PCM
  view.setUint16(off, 1, true);
  off += 2; // channels = 1
  view.setUint32(off, decoded.sampleRate, true);
  off += 4;
  view.setUint32(off, decoded.sampleRate * 2, true);
  off += 4; // byte rate
  view.setUint16(off, 2, true);
  off += 2; // block align
  view.setUint16(off, 16, true);
  off += 2; // bits/sample
  writeStr("data");
  view.setUint32(off, length * 2, true);
  off += 4;
  for (let i = 0; i < length; i++) {
    const s = Math.max(-1, Math.min(1, mono[i]));
    view.setInt16(off, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    off += 2;
  }
  return new Blob([buf], { type: "audio/wav" });
}

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
  img2imgAvailable = false,
  voiceInAvailable = false,
  personas = [],
  ref,
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
  // img2img toggle is independent of the picker model — when on, an
  // attached image gets edited by the dedicated img2img backend
  // (ComfyUI Kontext) regardless of whether the chat model has
  // image_gen caps. Only surfaced when the server is wired for it.
  const [img2img, setImg2img] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem(IMG2IMG_KEY) === "1";
    } catch {
      return false;
    }
  });
  // The img2img toggle is only meaningful when an image is actually
  // attached — without one the composer should behave like a plain chat
  // surface (model picker enabled, no refine/persona surface, etc.).
  // Keeping the toggle hidden until an attachment lands matches user
  // intent ("I want to edit this image") and avoids the trap where a
  // leftover localStorage flag locks an empty composer into image-mode.
  const showImg2imgToggle = img2imgAvailable && attached.length > 0;
  const toggleImg2img = () => {
    setImg2img((prev) => {
      const next = !prev;
      try {
        window.localStorage.setItem(IMG2IMG_KEY, next ? "1" : "0");
      } catch {
        // ignore
      }
      return next;
    });
  };
  // Effective send-time mode: img2img promotes to "image" only when an
  // image is actually attached. A stale toggle without an attachment
  // falls back to whatever the chat/image mode toggle says — almost
  // always plain chat.
  const effectiveMode: Mode = img2img && attached.length > 0 ? "image" : mode;
  const [refine, setRefine] = useState<boolean>(() => {
    try {
      const v = window.localStorage.getItem(REFINE_KEY);
      return v == null ? true : v === "1";
    } catch {
      return true;
    }
  });
  const showRefineToggle = effectiveMode === "image" && refinerAvailable;
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
    effectiveMode === "image" &&
    refinerAvailable &&
    refine &&
    personas.length > 0;
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

  type VoiceState = "idle" | "recording" | "transcribing";
  const [voiceState, setVoiceState] = useState<VoiceState>("idle");
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recorderStreamRef = useRef<MediaStream | null>(null);
  const recorderChunksRef = useRef<Blob[]>([]);

  const stopVoiceTracks = () => {
    recorderStreamRef.current?.getTracks().forEach((t) => t.stop());
    recorderStreamRef.current = null;
    recorderRef.current = null;
  };

  const pickRecorderMime = () => {
    if (typeof MediaRecorder === "undefined") return "";
    const candidates = [
      "audio/webm;codecs=opus",
      "audio/webm",
      "audio/ogg;codecs=opus",
      "audio/mp4",
    ];
    return candidates.find((t) => MediaRecorder.isTypeSupported(t)) ?? "";
  };

  const startVoice = async () => {
    if (voiceState !== "idle") return;
    setVoiceError(null);
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setVoiceError(`mic unavailable: ${msg}`);
      return;
    }
    recorderStreamRef.current = stream;
    recorderChunksRef.current = [];
    const mime = pickRecorderMime();
    let recorder: MediaRecorder;
    try {
      recorder = new MediaRecorder(stream, mime ? { mimeType: mime } : {});
    } catch (e) {
      stopVoiceTracks();
      setVoiceError(`recorder failed: ${String(e)}`);
      return;
    }
    recorderRef.current = recorder;
    recorder.ondataavailable = (e) => {
      if (e.data && e.data.size > 0) recorderChunksRef.current.push(e.data);
    };
    recorder.onstop = () => {
      const blobType = recorder.mimeType || mime || "audio/webm";
      const blob = new Blob(recorderChunksRef.current, { type: blobType });
      recorderChunksRef.current = [];
      stopVoiceTracks();
      void uploadVoice(blob);
    };
    recorder.start();
    setVoiceState("recording");
  };

  const uploadVoice = async (blob: Blob) => {
    if (blob.size === 0) {
      setVoiceState("idle");
      return;
    }
    setVoiceState("transcribing");
    try {
      // whisper.cpp's HTTP server decodes via dr_wav and rejects opus /
      // webm with a generic 400. Transcode here to 16 kHz mono PCM WAV
      // — the format whisper expects natively — using the Web Audio API.
      const wav = await encodeAsWav(blob);
      const res = await fetch("/api/transcribe", {
        method: "POST",
        credentials: "include",
        headers: { "content-type": wav.type },
        body: wav,
      });
      if (!res.ok) {
        const text = await res.text().catch(() => "");
        throw new Error(`${res.status} ${text}`);
      }
      const data = (await res.json()) as { text: string };
      const transcript = data.text?.trim() ?? "";
      if (transcript) {
        setValue((prev) => {
          const sep =
            prev && !prev.endsWith(" ") && !prev.endsWith("\n") ? " " : "";
          return prev + sep + transcript;
        });
        textareaRef.current?.focus();
      }
    } catch (e) {
      setVoiceError(`transcribe failed: ${String(e)}`);
    } finally {
      setVoiceState("idle");
    }
  };

  const stopVoice = () => {
    if (voiceState !== "recording") return;
    recorderRef.current?.stop();
  };

  const toggleVoice = () => {
    if (voiceState === "recording") stopVoice();
    else if (voiceState === "idle") void startVoice();
  };

  useEffect(() => {
    return () => {
      if (recorderRef.current && recorderRef.current.state !== "inactive") {
        try {
          recorderRef.current.stop();
        } catch {
          // ignore
        }
      }
      stopVoiceTracks();
    };
  }, []);

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
      effectiveMode,
      effectiveMode === "image" ? refine : undefined,
      effectiveMode === "image" && refine ? persona : undefined,
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
        const resized = await resizeImageForUpload(file, effectiveMode);
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

  // Attach affordances surface whenever an image can flow somewhere: a
  // vision-capable chat model, an image-gen model, or the img2img path
  // (ComfyUI Kontext, model-independent).
  const canAttach = vision || imageGen || img2imgAvailable;
  const onDragEnter = (e: React.DragEvent<HTMLDivElement>) => {
    if (!canAttach) return;
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    dragDepthRef.current++;
    setDragOver(true);
  };
  const onDragLeave = (e: React.DragEvent<HTMLDivElement>) => {
    if (!canAttach) return;
    e.preventDefault();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (dragDepthRef.current === 0) setDragOver(false);
  };
  const onDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!canAttach) return;
    if (!Array.from(e.dataTransfer.types).includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };
  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    if (!canAttach) return;
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

  useImperativeHandle(
    ref,
    () => ({
      remixWithImage: (image) => {
        setAttached([image]);
        setImg2img(true);
        try {
          window.localStorage.setItem(IMG2IMG_KEY, "1");
        } catch {
          // ignore
        }
        setValue("");
        setRecallIndex(-1);
        draftRef.current = "";
        // Defer focus until after the attachment chip has rendered so the
        // textarea doesn't fight with React's layout pass.
        requestAnimationFrame(() => textareaRef.current?.focus());
      },
    }),
    [],
  );

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
        {attached.length > 0 && img2img && img2imgAvailable && (
          <div
            css={{
              display: "inline-flex",
              alignSelf: "flex-start",
              alignItems: "center",
              gap: 6,
              padding: "3px 8px",
              margin: "4px 4px 0",
              borderRadius: 999,
              border: `1px solid ${theme.colors.border}`,
              background: theme.colors.activity.onSoft,
              color: theme.colors.text.main,
              ...theme.typography.caption,
            }}
          >
            <span
              className="material-icons-outlined"
              css={{
                fontSize: 14,
                color: theme.colors.activity.on,
              }}
            >
              auto_awesome
            </span>
            img2img · flux kontext
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
            {canAttach && (
              <>
                <input
                  ref={fileInputRef}
                  type="file"
                  accept="image/*"
                  multiple={mode === "chat"}
                  onChange={(e) => {
                    void onPickFiles(e.target.files);
                    e.target.value = "";
                  }}
                  css={{ display: "none" }}
                />
                <button
                  type="button"
                  aria-label={
                    mode === "image" ? "attach image to edit" : "attach image"
                  }
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
            {voiceInAvailable && (
              <button
                type="button"
                aria-label={
                  voiceState === "recording"
                    ? "stop recording"
                    : voiceState === "transcribing"
                      ? "transcribing"
                      : "record voice"
                }
                title={voiceError ?? "voice input"}
                disabled={voiceState === "transcribing"}
                aria-pressed={voiceState === "recording"}
                onClick={toggleVoice}
                css={{
                  ...composerSubButtonCss(theme),
                  color:
                    voiceState === "recording"
                      ? theme.colors.error
                      : voiceError
                        ? theme.colors.error
                        : theme.colors.text.muted,
                  ...(voiceState === "recording"
                    ? {
                        animation: "chat-mic-pulse 1.2s ease-in-out infinite",
                        "@keyframes chat-mic-pulse": {
                          "0%, 100%": { opacity: 0.55 },
                          "50%": { opacity: 1 },
                        },
                      }
                    : {}),
                }}
              >
                <span
                  className="material-icons-outlined"
                  css={{ fontSize: 22 }}
                >
                  {voiceState === "recording"
                    ? "stop"
                    : voiceState === "transcribing"
                      ? "graphic_eq"
                      : "mic"}
                </span>
              </button>
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
            {showImg2imgToggle && (
              <button
                type="button"
                aria-label={img2img ? "img2img: on" : "img2img: off"}
                title={
                  img2img
                    ? "img2img on — attached image will be edited via flux kontext"
                    : "img2img off — attached image will be analyzed by the chat model"
                }
                aria-pressed={img2img}
                onClick={toggleImg2img}
                css={{
                  ...composerSubButtonCss(theme),
                  color: img2img
                    ? theme.colors.activity.on
                    : theme.colors.text.muted,
                }}
              >
                <span
                  className="material-icons-outlined"
                  css={{ fontSize: 22 }}
                >
                  auto_awesome
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
              <ModelPicker
                value={model ?? null}
                onChange={onModelChange}
                disabled={img2img && attached.length > 0}
              />
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
    <>
      {open && (
        <div
          aria-hidden
          onClick={() => setOpen(false)}
          css={{
            position: "fixed",
            inset: 0,
            // Transparent on desktop — outside-pointer listener closes the
            // popup; the overlay only catches clicks that may not bubble
            // through scrollable parents. Dimmed on mobile so the
            // bottom-sheet feels anchored.
            background: "transparent",
            zIndex: 19,
            [mq[0]]: { background: "rgba(0,0,0,0.4)" },
          }}
        />
      )}
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
              [mq[0]]: {
                position: "fixed",
                left: 12,
                right: 12,
                bottom: 92,
                top: "auto",
                minWidth: 0,
                maxWidth: "none",
                maxHeight: "70vh",
                overflowY: "auto",
                padding: 8,
                borderRadius: 14,
              },
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
                    [mq[0]]: {
                      padding: "12px 14px",
                      gap: 4,
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
    </>
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
