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
import { resolveSttLang } from "../tts";
import MaskEditor, { MaskResult } from "./MaskEditor";
import ModelPicker from "./ModelPicker";

/**
 * Imperative handle parents can grab to seed the composer with an
 * incoming image — used by the "remix" action on a generated image to
 * pre-fill the attachment row and flip the img2img toggle without going
 * through the file picker again.
 */
export type ComposerHandle = {
  remixWithImage: (image: { base64: string; preview: string }) => void;
  /** Push a base image + already-drawn inpaint mask into the composer
   * in one shot. Used by the assistant-bubble "inpaint this" action
   * that routes through its own MaskEditor before handing the result
   * back here. Sets attachmentMode to "inpaint" and bypasses the
   * attached-change auto-clear so the freshly-paired mask survives
   * the next render. */
  pushInpaintWithMask: (
    image: { base64: string; preview: string },
    mask: MaskResult,
  ) => void;
};

type Mode = "chat" | "image";

/**
 * How an attached image is routed when image mode is active. "off" means
 * a vision-capable chat model just analyses the image. "edit" hands it to
 * the ComfyUI Kontext img2img workflow. "inpaint" routes to Flux Fill
 * with a user-drawn mask. Only the inpaint branch needs a mask payload —
 * "edit" alone is enough for Kontext, and "off" never reaches the image
 * pipeline at all.
 */
type AttachmentMode = "off" | "edit" | "inpaint";

type SubMode = "txt2img" | "img2img" | "inpaint";

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
    subMode?: SubMode,
    mask?: string,
    negative?: string,
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
  /** Whether the selected chat model can see attached images (vision).
   * Gates the "look only" attachment route and, with no ComfyUI host,
   * whether attaching is useful at all. */
  vision?: boolean;
  /** Whether the server has a prompt refiner model configured. */
  refinerAvailable?: boolean;
  /** Whether the server has a ComfyUI host configured. This is the
   * single gate for ALL image generation now — txt2img, img2img
   * (Kontext) and inpaint (Flux Fill) all run on it, independent of
   * the selected chat model. Drives the chat↔image mode control and
   * the attachment-routing segments. */
  img2imgAvailable?: boolean;
  /** Whether the server can transcribe audio (whisper.cpp endpoint
   * configured). Drives the mic affordance in the action row. */
  voiceInAvailable?: boolean;
  /** Available image-prompt personas. */
  personas?: Persona[];
  /**
   * When the chat tail is a completed image-gen reply, the route hands
   * its first image down here so the composer can pre-load it as the
   * seed for the next img2img turn. Auto-attached once per `id` —
   * removing the chip (or attaching a different image) doesn't trigger
   * a re-attach. New seeds (each successive image gen) take over.
   */
  suggestedSeed?: { id: string; url?: string; dataUrl?: string } | null;
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
const ATTACHMENT_MODE_KEY = "chat:attachmentMode";
// Legacy key kept around for the one-shot migration: "1" → edit, "0" → off.
const LEGACY_IMG2IMG_KEY = "chat:img2img";

/// One-shot read of the attachment-mode preference, with a migration
/// path from the old boolean `chat:img2img` flag. Older clients only
/// knew two modes (off / edit-via-Kontext); they're upgraded straight
/// to the new tri-state so the user's earlier toggle stays honoured.
const readAttachmentMode = (): AttachmentMode => {
  try {
    const stored = window.localStorage.getItem(ATTACHMENT_MODE_KEY);
    if (stored === "edit" || stored === "inpaint" || stored === "off") {
      return stored;
    }
    const legacy = window.localStorage.getItem(LEGACY_IMG2IMG_KEY);
    if (legacy === "1") return "edit";
    if (legacy === "0") return "off";
  } catch {
    // ignore — fall through to default.
  }
  return "off";
};

const writeAttachmentMode = (mode: AttachmentMode) => {
  try {
    window.localStorage.setItem(ATTACHMENT_MODE_KEY, mode);
    // Mirror to the legacy boolean so any code still reading it during
    // a transition window (e.g. SSR-hydrated sibling pages) gets the
    // right answer.
    window.localStorage.setItem(LEGACY_IMG2IMG_KEY, mode === "off" ? "0" : "1");
  } catch {
    // ignore storage errors (private mode, quota)
  }
};

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
  refinerAvailable = false,
  img2imgAvailable = false,
  voiceInAvailable = false,
  personas = [],
  suggestedSeed = null,
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
  // ComfyUI presence is the single gate for image generation now —
  // every chat model can drive it, so mode is a pure user choice that
  // no longer resets on model change.
  const imageGenAvailable = img2imgAvailable;
  const [mode, setMode] = useState<Mode>("chat");
  // Attachment-routing tri-state: "off" lets a vision chat model see
  // the image; "edit" hands it to ComfyUI Kontext (img2img); "inpaint"
  // pairs it with a user-drawn mask for the Flux Fill workflow. The
  // wrapper writes through to localStorage so the choice survives a
  // reload; raw useState setter is renamed off to silence the
  // "setter must be set<State>" lint and avoid accidentally bypassing
  // the persistence helper.
  // eslint-disable-next-line @eslint-react/use-state
  const [attachmentMode, persistAttachmentMode] =
    useState<AttachmentMode>(readAttachmentMode);
  const setAttachmentMode = (next: AttachmentMode) => {
    persistAttachmentMode(next);
    writeAttachmentMode(next);
  };
  const hasAttachment = attached.length > 0;
  // Inpaint is single-image only (Flux Fill expects exactly one base).
  // When the user piles on extra attachments, downshift to edit during
  // render so the composer doesn't sit in an unsubmittable state.
  // Render-phase setState mirrors the pattern already used for
  // `lastModelKey` in this file.
  if (attachmentMode === "inpaint" && attached.length > 1) {
    setAttachmentMode("edit");
  }
  // Keep attachmentMode coherent with what the current model + server
  // can actually do, so the unified mode control never lands on a
  // segment it isn't rendering: "look only" needs a vision model;
  // "edit"/"inpaint" need a ComfyUI host.
  if (hasAttachment) {
    if (attachmentMode === "off" && !vision && imageGenAvailable) {
      setAttachmentMode("edit");
    } else if (attachmentMode !== "off" && !imageGenAvailable) {
      setAttachmentMode("off");
    }
  }
  // Mask drawn by the user via MaskEditor. Cleared whenever the
  // attachment list changes (re-pick → re-mask), the toggle leaves
  // inpaint, or after a successful submit.
  const [mask, setMask] = useState<MaskResult | null>(null);
  const [maskEditorOpen, setMaskEditorOpen] = useState(false);
  // Flag flipped by `pushInpaintWithMask` so the upcoming
  // attached-change render doesn't auto-clear the freshly-paired mask.
  // Refs avoid kicking off a re-render of their own.
  const preserveMaskOnNextAttachedChangeRef = useRef(false);
  const attachedKey = attached.map((a) => a.preview).join("|");
  const [lastAttachedKey, setLastAttachedKey] = useState(attachedKey);
  if (lastAttachedKey !== attachedKey) {
    setLastAttachedKey(attachedKey);
    if (preserveMaskOnNextAttachedChangeRef.current) {
      preserveMaskOnNextAttachedChangeRef.current = false;
    } else {
      setMask(null);
    }
  }
  const [lastAttachmentMode, setLastAttachmentMode] = useState(attachmentMode);
  if (lastAttachmentMode !== attachmentMode) {
    setLastAttachmentMode(attachmentMode);
    if (attachmentMode !== "inpaint") setMask(null);
  }
  // Effective send-time mode. With an attachment the routing segment
  // decides: "look only" (off) is the vision-chat path, edit/inpaint
  // are image gen. With no attachment it's the plain chat↔image choice.
  const effectiveMode: Mode = hasAttachment
    ? attachmentMode === "off"
      ? "chat"
      : "image"
    : mode;
  // Routing only means anything with an image in hand. Without one the
  // persisted attachmentMode (e.g. a stale "inpaint" from a prior turn)
  // is irrelevant — collapse it to "off" so txt2img isn't mistaken for
  // an unsatisfiable inpaint and the send button stays live.
  const routeMode: AttachmentMode = hasAttachment ? attachmentMode : "off";
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
  // Negative-prompt override. Effective on the real-CFG paths: Z-Image
  // txt2img (cfg 2.0) and Flux Fill inpaint (cfg 3.5). Kontext img2img
  // runs at cfg=1 and ignores it, so we hide the control there to avoid
  // implying it does something. Surfaced behind a "show negative"
  // toggle so it doesn't crowd the basic image-prompt flow.
  const [negative, setNegative] = useState("");
  const [negativeOpen, setNegativeOpen] = useState(false);
  const showNegativeToggle =
    effectiveMode === "image" && !(hasAttachment && attachmentMode === "edit");
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

  type VoiceState = "idle" | "listening" | "speaking" | "stopping";
  const [voiceState, setVoiceState] = useState<VoiceState>("idle");
  const [voiceError, setVoiceError] = useState<string | null>(null);
  // Number of segments queued / in flight. Drives the small "…" badge
  // so the user knows tail audio is still being committed after stop.
  const [voicePending, setVoicePending] = useState(0);
  const voiceStreamRef = useRef<MediaStream | null>(null);
  // Each utterance gets its own MediaRecorder so the resulting blob is
  // a self-contained WebM (not a mid-stream chunk that's unplayable
  // without the init segment). On VAD silence we stop the current
  // recorder, hand the blob to the transcribe queue, and spin a fresh
  // recorder for the next utterance.
  const segmentRecorderRef = useRef<MediaRecorder | null>(null);
  const segmentChunksRef = useRef<Blob[]>([]);
  const segmentMimeRef = useRef<string>("");
  // True once the VAD has flagged this segment as actual speech.
  // Stays false while we're only hearing ambient noise or short
  // transients like a touchpad click — the segment is then dropped
  // instead of going to whisper, so phantom utterances don't land in
  // the textarea every time the user stops.
  const segmentHadSpeechRef = useRef(false);
  // VAD plumbing — analyser samples input RMS so we can detect speech
  // vs silence in pure Web Audio, no extra dependency.
  const vadCtxRef = useRef<AudioContext | null>(null);
  const vadAnalyserRef = useRef<AnalyserNode | null>(null);
  const vadFrameRef = useRef<number | null>(null);
  // True after the user hits stop; the segment recorder's `onstop`
  // checks this so it doesn't spin up another segment.
  const voiceFinalizingRef = useRef(false);
  // Serial queue: each finalized blob is appended to this chain so
  // transcripts land in speech order even if a later one returns
  // faster than an earlier one.
  const voiceQueueRef = useRef<Promise<void>>(Promise.resolve());
  // Snapshot of the textarea contents at recording start, kept for
  // bookkeeping symmetry with the previous single-shot path.
  const voiceBaseRef = useRef<string>("");

  // VAD tuning. RMS over [0,1]; default mic gain on most laptops sits
  // around 0.02–0.05 for ambient noise and 0.1+ for speech. Need at
  // least 250 ms of audio above the threshold before we'll treat it
  // as speech — kills cough / keyboard tap false starts. End an
  // utterance after 700 ms of silence following speech.
  const VAD_RMS_THRESHOLD = 0.04;
  const VAD_MIN_SPEECH_MS = 250;
  const VAD_SILENCE_MS = 700;

  const stopVoiceTracks = () => {
    voiceStreamRef.current?.getTracks().forEach((t) => t.stop());
    voiceStreamRef.current = null;
  };

  const stopVadLoop = () => {
    if (vadFrameRef.current != null) {
      window.cancelAnimationFrame(vadFrameRef.current);
      vadFrameRef.current = null;
    }
    vadAnalyserRef.current = null;
    if (vadCtxRef.current) {
      void vadCtxRef.current.close().catch(() => {});
      vadCtxRef.current = null;
    }
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

  /// Push a finalized utterance blob onto the serial transcribe queue.
  /// Each task awaits the previous so transcripts land in speech order
  /// even if one POST is slower than the next.
  const enqueueTranscribe = (blob: Blob) => {
    if (blob.size === 0) return;
    setVoicePending((n) => n + 1);
    voiceQueueRef.current = voiceQueueRef.current
      .then(async () => {
        try {
          const wav = await encodeAsWav(blob);
          const lang = resolveSttLang();
          const url = lang
            ? `/api/transcribe?lang=${encodeURIComponent(lang)}`
            : "/api/transcribe";
          const res = await fetch(url, {
            method: "POST",
            credentials: "include",
            headers: { "content-type": wav.type },
            body: wav,
          });
          if (!res.ok) return;
          const data = (await res.json()) as { text: string };
          const transcript = data.text?.trim() ?? "";
          if (transcript) {
            setValue((prev) => {
              const sep =
                prev && !prev.endsWith(" ") && !prev.endsWith("\n") ? " " : "";
              return prev + sep + transcript;
            });
          }
        } catch (e) {
          // Per-segment failures don't poison the queue.
          console.error("segment transcribe failed", e);
        }
      })
      .finally(() => {
        setVoicePending((n) => Math.max(0, n - 1));
      });
  };

  const startSegmentRecorder = (stream: MediaStream) => {
    if (!segmentMimeRef.current) {
      segmentMimeRef.current = pickRecorderMime();
    }
    segmentChunksRef.current = [];
    segmentHadSpeechRef.current = false;
    const mime = segmentMimeRef.current;
    let recorder: MediaRecorder;
    try {
      recorder = new MediaRecorder(stream, mime ? { mimeType: mime } : {});
    } catch (e) {
      setVoiceError(`recorder failed: ${String(e)}`);
      return;
    }
    recorder.ondataavailable = (e) => {
      if (e.data && e.data.size > 0) segmentChunksRef.current.push(e.data);
    };
    recorder.onstop = () => {
      const blobMime = recorder.mimeType || mime || "audio/webm";
      const chunks = segmentChunksRef.current.slice();
      segmentChunksRef.current = [];
      const hadSpeech = segmentHadSpeechRef.current;
      segmentHadSpeechRef.current = false;
      const blob = new Blob(chunks, { type: blobMime });
      // Only commit segments the VAD actually saw speech in. The
      // stop button itself produces a short click that would
      // otherwise reach whisper as a phantom utterance.
      if (hadSpeech && blob.size > 0) enqueueTranscribe(blob);
      const liveStream = voiceStreamRef.current;
      if (!voiceFinalizingRef.current && liveStream) {
        startSegmentRecorder(liveStream);
      }
    };
    recorder.start();
    segmentRecorderRef.current = recorder;
  };

  /// Walk the analyser's RMS energy. Speech starts when energy stays
  /// above threshold for `VAD_MIN_SPEECH_MS`; it ends after
  /// `VAD_SILENCE_MS` of sub-threshold audio. End-of-utterance stops
  /// the current segment recorder, which hands off the blob and
  /// chains the next recorder.
  const startVadLoop = (stream: MediaStream) => {
    const AC =
      window.AudioContext ??
      (window as unknown as { webkitAudioContext: typeof AudioContext })
        .webkitAudioContext;
    const ctx = new AC();
    vadCtxRef.current = ctx;
    const source = ctx.createMediaStreamSource(stream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 1024;
    analyser.smoothingTimeConstant = 0;
    source.connect(analyser);
    vadAnalyserRef.current = analyser;
    const data = new Uint8Array(analyser.frequencyBinCount);
    let speechStartedAt: number | null = null;
    let silenceStartedAt: number | null = null;
    const tick = () => {
      const a = vadAnalyserRef.current;
      if (!a) return;
      a.getByteTimeDomainData(data);
      let sum = 0;
      for (let i = 0; i < data.length; i++) {
        const v = (data[i] - 128) / 128;
        sum += v * v;
      }
      const rms = Math.sqrt(sum / data.length);
      const now = performance.now();
      if (rms > VAD_RMS_THRESHOLD) {
        silenceStartedAt = null;
        if (speechStartedAt == null) speechStartedAt = now;
        if (now - speechStartedAt >= VAD_MIN_SPEECH_MS) {
          segmentHadSpeechRef.current = true;
          setVoiceState((s) => (s === "listening" ? "speaking" : s));
        }
      } else {
        if (silenceStartedAt == null) silenceStartedAt = now;
        if (
          speechStartedAt != null &&
          now - speechStartedAt >= VAD_MIN_SPEECH_MS &&
          now - silenceStartedAt >= VAD_SILENCE_MS
        ) {
          speechStartedAt = null;
          silenceStartedAt = null;
          setVoiceState((s) => (s === "speaking" ? "listening" : s));
          const r = segmentRecorderRef.current;
          if (r && r.state === "recording") {
            try {
              r.stop();
            } catch {
              // ignore — onstop will fire either way
            }
          }
        }
      }
      vadFrameRef.current = window.requestAnimationFrame(tick);
    };
    vadFrameRef.current = window.requestAnimationFrame(tick);
  };

  const startVoice = async () => {
    if (voiceState !== "idle") return;
    setVoiceError(null);
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (e) {
      setVoiceError(`mic unavailable: ${String(e)}`);
      return;
    }
    voiceStreamRef.current = stream;
    voiceBaseRef.current = value;
    voiceFinalizingRef.current = false;
    segmentMimeRef.current = pickRecorderMime();
    startSegmentRecorder(stream);
    startVadLoop(stream);
    setVoiceState("listening");
  };

  const stopVoice = () => {
    if (voiceState === "idle" || voiceState === "stopping") return;
    voiceFinalizingRef.current = true;
    stopVadLoop();
    setVoiceState("stopping");
    const r = segmentRecorderRef.current;
    if (r && r.state === "recording") {
      try {
        r.stop();
      } catch {
        // ignore
      }
    }
    void voiceQueueRef.current
      .catch(() => {})
      .then(() => {
        segmentRecorderRef.current = null;
        stopVoiceTracks();
        setVoiceState("idle");
        textareaRef.current?.focus();
      });
  };

  const toggleVoice = () => {
    if (voiceState === "idle") void startVoice();
    else if (voiceState !== "stopping") stopVoice();
  };

  useEffect(() => {
    return () => {
      voiceFinalizingRef.current = true;
      stopVadLoop();
      const r = segmentRecorderRef.current;
      if (r && r.state === "recording") {
        try {
          r.stop();
        } catch {
          // ignore
        }
      }
      segmentRecorderRef.current = null;
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
    // Inpaint without a mask is a guaranteed 400 on the backend. Block
    // at the boundary so the user gets the hint instead of a silent
    // request failure.
    if (effectiveMode === "image" && routeMode === "inpaint" && !mask) {
      return;
    }
    const imgs =
      attached.length > 0 ? attached.map((a) => a.base64) : undefined;
    let subMode: SubMode | undefined;
    if (effectiveMode === "image") {
      if (routeMode === "inpaint") subMode = "inpaint";
      else if (routeMode === "edit") subMode = "img2img";
      else if (!imgs) subMode = "txt2img";
    }
    const maskPayload =
      effectiveMode === "image" && routeMode === "inpaint" && mask
        ? mask.base64
        : undefined;
    const negativeTrim = negative.trim();
    const negativePayload =
      effectiveMode === "image" && negativeTrim.length > 0
        ? negativeTrim
        : undefined;
    onSend(
      trimmed,
      imgs,
      effectiveMode,
      effectiveMode === "image" ? refine : undefined,
      effectiveMode === "image" && refine ? persona : undefined,
      subMode,
      maskPayload,
      negativePayload,
    );
    setValue("");
    setAttached([]);
    setMask(null);
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
  // vision-capable chat model can look at it, or a ComfyUI host can
  // edit/inpaint it (model-independent).
  const canAttach = vision || imageGenAvailable;

  // Unified mode control. With no attachment it's a plain chat↔image
  // switch (shown only when ComfyUI can actually generate). With an
  // attachment it becomes the routing picker — look (vision chat) /
  // edit (Kontext) / inpaint (Flux Fill) — each segment gated on the
  // capability that backs it. One control, context-dependent segments,
  // replacing the old separate mode toggle + attachment-mode pill.
  type ModeSegment = { value: string; icon: string; label: string };
  let modeSegments: ModeSegment[];
  let modeValue: string;
  let onModeSegment: (v: string) => void;
  if (hasAttachment) {
    const segs: ModeSegment[] = [];
    if (vision) {
      segs.push({ value: "off", icon: "visibility", label: "look only" });
    }
    if (imageGenAvailable) {
      segs.push({
        value: "edit",
        icon: "auto_awesome",
        label: "edit (img2img)",
      });
      if (attached.length === 1) {
        segs.push({ value: "inpaint", icon: "brush", label: "inpaint mask" });
      }
    }
    modeSegments = segs;
    modeValue = attachmentMode;
    onModeSegment = (v) => setAttachmentMode(v as AttachmentMode);
  } else {
    modeSegments = [
      { value: "chat", icon: "chat_bubble_outline", label: "chat" },
      { value: "image", icon: "image", label: "image" },
    ];
    modeValue = mode;
    onModeSegment = (v) => setMode(v as Mode);
  }
  const showModeControl = hasAttachment
    ? modeSegments.length > 1
    : imageGenAvailable;

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

  // Pre-fill the composer with the chat's last generated image so the
  // next prompt defaults to another img2img turn. Each seed `id` is
  // consumed once — once attached (or once the user types / attaches
  // their own image instead), the same id won't re-trigger. A new gen
  // produces a fresh id and replaces the chain. `consumedSeedRef` is
  // a ref (not state) so the consumption flag doesn't tick a re-render
  // and re-evaluate this effect right after the setAttached lands.
  const consumedSeedRef = useRef<string | null>(null);
  useEffect(() => {
    const seed = suggestedSeed;
    if (!seed) return;
    if (consumedSeedRef.current === seed.id) return;
    // Don't override an in-progress draft: if there's any user input
    // already (attachment or text), assume they're mid-composition.
    if (attached.length > 0 || value.length > 0) {
      consumedSeedRef.current = seed.id;
      return;
    }
    if (streaming) return;
    let cancelled = false;
    void (async () => {
      try {
        let base64 = "";
        let preview = "";
        if (seed.dataUrl) {
          preview = seed.dataUrl;
          const comma = preview.indexOf(",");
          base64 = comma >= 0 ? preview.slice(comma + 1) : "";
        } else if (seed.url) {
          const blob = await (
            await fetch(seed.url, { credentials: "include" })
          ).blob();
          preview = await new Promise<string>((resolve, reject) => {
            const r = new FileReader();
            r.onload = () => resolve(String(r.result));
            r.onerror = () => reject(r.error);
            r.readAsDataURL(blob);
          });
          const comma = preview.indexOf(",");
          base64 = comma >= 0 ? preview.slice(comma + 1) : "";
        }
        if (cancelled || !base64) return;
        consumedSeedRef.current = seed.id;
        setAttached([{ base64, preview }]);
        // Chain defaults to plain img2img (edit). Inpaint requires
        // the user to actively draw a mask, so we never force them
        // into inpaint mode automatically — they'd just hit send with
        // no mask and get a 400.
        setAttachmentMode("edit");
      } catch (e) {
        console.error("chain seed fetch failed", e);
      }
    })();
    return () => {
      cancelled = true;
    };
    // setAttachmentMode is stable; intentionally not in deps.
  }, [suggestedSeed, attached.length, value.length, streaming]);

  useImperativeHandle(
    ref,
    () => ({
      remixWithImage: (image) => {
        setAttached([image]);
        setAttachmentMode("edit");
        setMask(null);
        setValue("");
        setRecallIndex(-1);
        draftRef.current = "";
        // Defer focus until after the attachment chip has rendered so the
        // textarea doesn't fight with React's layout pass.
        requestAnimationFrame(() => textareaRef.current?.focus());
      },
      pushInpaintWithMask: (image, m) => {
        preserveMaskOnNextAttachedChangeRef.current = true;
        setAttached([image]);
        setAttachmentMode("inpaint");
        setMask(m);
        setValue("");
        setRecallIndex(-1);
        draftRef.current = "";
        requestAnimationFrame(() => textareaRef.current?.focus());
      },
    }),
    [],
  );

  const inpaintReady =
    !(effectiveMode === "image" && routeMode === "inpaint") || !!mask;
  const canSend =
    (!!value.trim() || attached.length > 0) && !streaming && inpaintReady;

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
              className="material-symbols-outlined"
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
        {attached.length > 0 &&
          attachmentMode !== "off" &&
          img2imgAvailable && (
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
                className="material-symbols-outlined"
                css={{
                  fontSize: 14,
                  color: theme.colors.activity.on,
                }}
              >
                {attachmentMode === "inpaint" ? "brush" : "auto_awesome"}
              </span>
              {attachmentMode === "inpaint"
                ? mask
                  ? "inpaint · flux fill · mask ready"
                  : "inpaint · draw a mask to enable send"
                : "img2img · flux kontext"}
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
            {attached.map((a, i) => {
              // Swap the thumbnail to the masked composite when the
              // user has painted one; otherwise the chip would show
              // the unaltered base and the user couldn't tell whether
              // a mask was attached without re-opening the editor.
              const previewSrc =
                i === 0 && mask && attachmentMode === "inpaint"
                  ? mask.preview
                  : a.preview;
              return (
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
                    src={previewSrc}
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
                      className="material-symbols-outlined"
                      css={{ fontSize: 12 }}
                    >
                      close
                    </span>
                  </button>
                </div>
              );
            })}
          </div>
        )}
        {showNegativeToggle && negativeOpen && (
          <div
            css={{
              padding: "4px 4px 0",
              display: "flex",
              flexDirection: "column",
              gap: 4,
            }}
          >
            <label
              htmlFor="composer-negative"
              css={{
                ...theme.typography.caption,
                color: theme.colors.text.muted,
                fontFamily: theme.fonts.heading,
              }}
            >
              negative — things to avoid (comma-separated)
            </label>
            <textarea
              id="composer-negative"
              value={negative}
              onChange={(e) => setNegative(e.target.value)}
              placeholder="e.g. extra fingers, blurry, deformed paws"
              rows={2}
              css={{
                border: `1px solid ${theme.colors.border}`,
                outline: "none",
                background: theme.colors.background.main,
                resize: "none",
                width: "100%",
                padding: "6px 8px",
                borderRadius: 8,
                ...theme.typography.caption,
                color: theme.colors.text.main,
                lineHeight: 1.4,
                "&:focus": {
                  borderColor: theme.colors.text.muted,
                },
                "&::placeholder": { color: theme.colors.text.muted },
              }}
            />
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
                  multiple={attachmentMode !== "inpaint"}
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
                    className="material-symbols-outlined"
                    css={{ fontSize: 22 }}
                  >
                    add
                  </span>
                </button>
              </>
            )}
            {voiceInAvailable && (
              <>
                <button
                  type="button"
                  aria-label={
                    voiceState === "idle"
                      ? "record voice"
                      : voiceState === "stopping"
                        ? "finalising"
                        : "stop recording"
                  }
                  title={voiceError ?? "voice input"}
                  disabled={voiceState === "stopping"}
                  aria-pressed={voiceState !== "idle"}
                  onClick={toggleVoice}
                  css={{
                    ...composerSubButtonCss(theme),
                    color:
                      voiceState === "speaking"
                        ? theme.colors.error
                        : voiceState === "listening"
                          ? theme.colors.activity.on
                          : voiceError
                            ? theme.colors.error
                            : theme.colors.text.muted,
                    ...(voiceState === "speaking"
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
                    className="material-symbols-outlined"
                    css={{ fontSize: 22 }}
                  >
                    {voiceState === "idle"
                      ? "mic"
                      : voiceState === "stopping"
                        ? "graphic_eq"
                        : "stop"}
                  </span>
                </button>
                {voiceState !== "idle" && (
                  <span
                    aria-live="polite"
                    css={{
                      ...theme.typography.caption,
                      fontFamily:
                        "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
                      color:
                        voiceState === "speaking"
                          ? theme.colors.error
                          : voiceState === "listening"
                            ? theme.colors.activity.on
                            : theme.colors.text.muted,
                      minWidth: 64,
                    }}
                  >
                    {voiceState === "speaking"
                      ? "speaking…"
                      : voiceState === "listening"
                        ? voicePending > 0
                          ? `listening · ${voicePending}`
                          : "listening"
                        : voicePending > 0
                          ? `finalising · ${voicePending}`
                          : "finalising"}
                  </span>
                )}
              </>
            )}
            {showModeControl && (
              <SegmentedToggle
                ariaLabel={hasAttachment ? "attachment mode" : "send mode"}
                segments={modeSegments}
                value={modeValue}
                onChange={onModeSegment}
                theme={theme}
              />
            )}
            {hasAttachment && attachmentMode === "inpaint" && (
              <button
                type="button"
                aria-label={mask ? "edit mask" : "draw mask"}
                title={
                  mask
                    ? "edit mask — open the painter to refine the region"
                    : "draw mask — pick the region to repaint"
                }
                onClick={() => setMaskEditorOpen(true)}
                css={{
                  ...composerSubButtonCss(theme),
                  color: mask
                    ? theme.colors.activity.on
                    : theme.colors.text.muted,
                }}
              >
                <span
                  className="material-symbols-outlined"
                  css={{ fontSize: 22 }}
                >
                  {mask ? "brush" : "edit"}
                </span>
              </button>
            )}
            {showNegativeToggle && (
              <button
                type="button"
                aria-label={
                  negativeOpen ? "hide negative prompt" : "show negative prompt"
                }
                title={
                  negative.trim().length > 0
                    ? "negative prompt set — click to edit"
                    : "add a negative prompt (things to avoid)"
                }
                aria-pressed={negativeOpen}
                onClick={() => setNegativeOpen((v) => !v)}
                css={{
                  ...composerSubButtonCss(theme),
                  color:
                    negative.trim().length > 0
                      ? theme.colors.activity.on
                      : theme.colors.text.muted,
                }}
              >
                <span
                  className="material-symbols-outlined"
                  css={{ fontSize: 22 }}
                >
                  block
                </span>
              </button>
            )}
            {showRefineToggle && (
              <RefineControl
                refine={refine}
                onToggleRefine={toggleRefine}
                personas={personas}
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
                disabled={effectiveMode === "image"}
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
                  className="material-symbols-outlined"
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
                  className="material-symbols-outlined"
                  css={{ fontSize: 20 }}
                >
                  arrow_upward
                </span>
              </button>
            )}
          </div>
        </div>
      </div>
      {maskEditorOpen && attached[0] && (
        <MaskEditor
          imageSrc={attached[0].preview}
          onCancel={() => setMaskEditorOpen(false)}
          onDone={(m) => {
            setMask(m);
            setMaskEditorOpen(false);
          }}
        />
      )}
    </div>
  );
};

const SegmentedToggle = ({
  segments,
  value,
  onChange,
  ariaLabel,
  theme,
}: {
  segments: { value: string; icon: string; label: string }[];
  value: string;
  onChange: (next: string) => void;
  ariaLabel: string;
  theme: Theme;
}) => {
  // Generic icon segmented pill. Backs both the chat↔image switch and
  // the attachment-routing picker (look / edit / inpaint) — the caller
  // supplies the segment set for the current context.
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      css={{
        display: "inline-flex",
        alignItems: "center",
        padding: 2,
        gap: 2,
        borderRadius: 8,
        background: theme.colors.background.main,
        border: `1px solid ${theme.colors.border}`,
      }}
    >
      {segments.map((c) => {
        const active = value === c.value;
        return (
          <button
            key={c.value}
            type="button"
            role="radio"
            aria-checked={active}
            aria-label={c.label}
            title={c.label}
            onClick={() => onChange(c.value)}
            css={{
              width: 28,
              height: 26,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              border: "none",
              borderRadius: 6,
              background: active ? theme.colors.activity.onSoft : "transparent",
              color: active
                ? theme.colors.activity.on
                : theme.colors.text.muted,
              cursor: "pointer",
            }}
          >
            <span className="material-symbols-outlined" css={{ fontSize: 18 }}>
              {c.icon}
            </span>
          </button>
        );
      })}
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
  const accent = refine ? theme.colors.activity.on : theme.colors.text.muted;
  const hasPersonas = personas.length > 0;

  // The menu opens upward from a button pinned near the bottom of the
  // viewport, so its ceiling is the space *above* the button — not the
  // full page height. Measure the button's top on open (and on resize)
  // and cap the menu to that, scrolling any overflow. Falls back to a
  // viewport-relative guess on the first synchronous paint.
  const [maxMenuH, setMaxMenuH] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!open) return;
    const measure = () => {
      const el = wrapRef.current;
      if (!el) return;
      // 8px gap above the button + an 8px breathing margin at the top.
      // Measuring requires the laid-out DOM, so the synchronous set in
      // this layout effect is intentional — it runs before paint, no flash.
      // eslint-disable-next-line @eslint-react/set-state-in-effect
      setMaxMenuH(
        Math.max(160, Math.floor(el.getBoundingClientRect().top - 16)),
      );
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [open]);

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
      <div ref={wrapRef} css={{ position: "relative", display: "inline-flex" }}>
        {/* One affordance: opens a popover holding both the refine
            on/off switch and (when configured) the persona picker.
            No hover-only chevron — everything lives behind a click. */}
        <button
          type="button"
          aria-label="image prompt options"
          aria-haspopup="menu"
          aria-expanded={open}
          title={
            refine
              ? "prompt refiner on — click for options"
              : "prompt refiner off — click for options"
          }
          onClick={() => setOpen((v) => !v)}
          css={{ ...composerSubButtonCss(theme), color: accent }}
        >
          <span className="material-symbols-outlined" css={{ fontSize: 22 }}>
            auto_fix_high
          </span>
        </button>
        {open && (
          <div
            role="menu"
            css={{
              position: "absolute",
              bottom: "calc(100% + 8px)",
              left: 0,
              minWidth: 280,
              // Wider on desktop for readable persona descriptions,
              // clamped to the viewport so it never overflows sideways.
              maxWidth: "min(440px, calc(100vw - 24px))",
              // Cap to the measured space above the button so the
              // upward-opening menu never runs off the top; scroll the
              // overflow. mq[0] below overrides for the mobile sheet.
              maxHeight: maxMenuH ? `${maxMenuH}px` : "calc(100vh - 120px)",
              overflowY: "auto",
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
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={refine}
              onClick={onToggleRefine}
              css={{
                textAlign: "left",
                border: "none",
                borderRadius: 8,
                padding: "8px 10px",
                background: "transparent",
                color: theme.colors.text.main,
                cursor: "pointer",
                display: "flex",
                alignItems: "center",
                gap: 10,
                "&:hover": { background: theme.colors.background.light },
                [mq[0]]: { padding: "12px 14px" },
              }}
            >
              <div
                css={{
                  display: "flex",
                  flexDirection: "column",
                  gap: 2,
                  flex: 1,
                }}
              >
                <span css={{ ...theme.typography.body2, fontWeight: 600 }}>
                  refine prompt
                </span>
                <span
                  css={{
                    ...theme.typography.caption,
                    color: theme.colors.text.muted,
                  }}
                >
                  {refine
                    ? "model expands your prompt before generating"
                    : "your prompt is sent to the image model as-is"}
                </span>
              </div>
              <span
                className="material-symbols-outlined"
                css={{ fontSize: 28, color: accent }}
              >
                {refine ? "toggle_on" : "toggle_off"}
              </span>
            </button>
            {hasPersonas && (
              <>
                <div
                  css={{
                    height: 1,
                    background: theme.colors.border,
                    margin: "4px 0",
                  }}
                />
                <span
                  css={{
                    ...theme.typography.caption,
                    color: theme.colors.text.muted,
                    fontFamily: theme.fonts.heading,
                    padding: "2px 10px",
                  }}
                >
                  persona{!refine && " · enable refine to use"}
                </span>
                {personas.map((p) => {
                  const selected = p.id === persona;
                  return (
                    <button
                      key={p.id}
                      type="button"
                      role="menuitemradio"
                      aria-checked={selected}
                      disabled={!refine}
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
                        cursor: refine ? "pointer" : "not-allowed",
                        opacity: refine ? 1 : 0.45,
                        display: "flex",
                        flexDirection: "column",
                        gap: 2,
                        "&:hover": {
                          background:
                            selected || !refine
                              ? selected
                                ? theme.colors.activity.onSoft
                                : "transparent"
                              : theme.colors.background.light,
                        },
                        [mq[0]]: { padding: "12px 14px", gap: 4 },
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
              </>
            )}
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
