import { useTheme } from "@emotion/react";
import {
  PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState,
} from "react";

/**
 * Output sent to the parent on "done" — the mask PNG as base64 (no
 * `data:` prefix) plus the natural pixel dimensions so the caller can
 * sanity-check alignment with the base image before sending. ComfyUI's
 * `LoadImageMask` reads the red channel, so the encode-time composite
 * is a black canvas with white-painted strokes.
 */
export type MaskResult = {
  /** Binary mask PNG, base64 (no `data:` prefix). White = repaint,
   * black = keep. Fed directly to Flux Fill's LoadImageMask. */
  base64: string;
  /** Data URL preview of the base image with the mask painted on top
   * in translucent red. Used by the composer thumbnail so the user
   * can see what they masked without re-opening the editor. */
  preview: string;
  width: number;
  height: number;
};

type Props = {
  /** Data URL for the base image — what the user is masking. */
  imageSrc: string;
  /** Closes the editor without producing a mask. */
  onCancel: () => void;
  /** Fires when the user commits the mask. The component does the
   * composite + base64 encode itself so the parent doesn't have to. */
  onDone: (mask: MaskResult) => void;
};

type Tool = "brush" | "eraser";

const MIN_BRUSH = 6;
const MAX_BRUSH = 160;
const DEFAULT_BRUSH = 40;
const MAX_UNDO_STEPS = 32;

// Translucent red for live painting — low enough alpha that the user
// can still see what's underneath the brush, opaque enough to read
// against bright and dark backgrounds. The commit pass thresholds
// painted pixels to a binary mask, so this alpha only affects the
// in-editor preview, never the PNG handed to Flux Fill.
const STROKE_COLOR = "rgba(255, 56, 56, 0.45)";

/**
 * Full-screen masked-inpaint editor. The base image renders behind a
 * transparent canvas that captures pointer strokes; the canvas matches
 * the image's natural pixel dimensions so each stroke lands at full
 * resolution regardless of viewport scale. Pointer-event coordinates
 * are translated through the canvas's bounding rect so a 1024×1024
 * image can be painted on a 400px-wide phone without losing precision.
 */
const MaskEditor = ({ imageSrc, onCancel, onDone }: Props) => {
  const theme = useTheme();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  // Ref onto the visible base image — drawImage uses it on commit to
  // composite a preview thumbnail for the composer.
  const baseImgRef = useRef<HTMLImageElement>(null);
  const [tool, setTool] = useState<Tool>("brush");
  const [brushSize, setBrushSize] = useState<number>(DEFAULT_BRUSH);
  const [naturalSize, setNaturalSize] = useState<{
    width: number;
    height: number;
  } | null>(null);
  const [hasStroke, setHasStroke] = useState(false);

  // Refs hold drawing state so pointer-handler closures aren't
  // re-created on every brush-size tweak — the handlers read these
  // through .current rather than capturing values.
  const drawingRef = useRef(false);
  const lastPointRef = useRef<{ x: number; y: number } | null>(null);
  const toolRef = useRef<Tool>(tool);
  const brushRef = useRef<number>(brushSize);
  useEffect(() => {
    toolRef.current = tool;
  }, [tool]);
  useEffect(() => {
    brushRef.current = brushSize;
  }, [brushSize]);

  // Bounded undo stack. Each entry is a full canvas snapshot taken
  // right before a stroke starts. ImageData keeps full fidelity (no
  // PNG re-encode loss between undos) and bypasses ToDataURL's CORS
  // tainting check entirely.
  const undoStackRef = useRef<ImageData[]>([]);
  const [undoDepth, setUndoDepth] = useState(0);

  // Probe the image's natural dimensions before the canvas mounts.
  // A hidden <img> with display:none doesn't reliably fire onload
  // across browsers, so load via the Image constructor instead.
  // The canvas's intrinsic width/height come from React props derived
  // from naturalSize; mutating them imperatively would wipe strokes
  // on every render, so we don't touch canvas.width here.
  useEffect(() => {
    const img = new Image();
    let cancelled = false;
    img.onload = () => {
      if (cancelled) return;
      const w = img.naturalWidth || img.width;
      const h = img.naturalHeight || img.height;
      setNaturalSize({ width: w, height: h });
    };
    img.src = imageSrc;
    return () => {
      cancelled = true;
    };
  }, [imageSrc]);

  // Clear undo stack when the canvas resolution swaps. The size change
  // re-renders the canvas with new width/height attributes which the
  // browser also uses to clear its bitmap — so any stashed ImageData
  // from a previous resolution is no longer applicable. Compare-prev
  // pattern dodges the "setState in effect" lint without losing
  // correctness; React schedules this with the same render.
  const naturalSizeKey = naturalSize
    ? `${naturalSize.width}x${naturalSize.height}`
    : "";
  const [lastNaturalSizeKey, setLastNaturalSizeKey] = useState(naturalSizeKey);
  if (lastNaturalSizeKey !== naturalSizeKey) {
    setLastNaturalSizeKey(naturalSizeKey);
    undoStackRef.current = [];
    setUndoDepth(0);
    setHasStroke(false);
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
      } else if ((e.metaKey || e.ctrlKey) && e.key === "z") {
        e.preventDefault();
        undo();
      } else if (e.key === "[") {
        setBrushSize((s) => Math.max(MIN_BRUSH, Math.round(s * 0.85)));
      } else if (e.key === "]") {
        setBrushSize((s) => Math.min(MAX_BRUSH, Math.round(s * 1.15)));
      } else if (e.key === "b" || e.key === "B") {
        setTool("brush");
      } else if (e.key === "e" || e.key === "E") {
        setTool("eraser");
      }
    };
    window.addEventListener("keydown", onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
    };
    // onCancel + undo are stable enough not to thrash this listener
    // every render; intentionally not in the dep array.
    // eslint-disable-next-line @eslint-react/exhaustive-deps
  }, []);

  const snapshot = () => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const data = ctx.getImageData(0, 0, canvas.width, canvas.height);
    const stack = undoStackRef.current;
    stack.push(data);
    if (stack.length > MAX_UNDO_STEPS) stack.shift();
    setUndoDepth(stack.length);
  };

  const undo = () => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    const data = undoStackRef.current.pop();
    setUndoDepth(undoStackRef.current.length);
    if (!data) {
      // Stack empty → nothing to revert past, just clear so the user
      // gets back to a blank canvas instead of being stuck with their
      // first stroke.
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      setHasStroke(false);
      return;
    }
    ctx.putImageData(data, 0, 0);
    setHasStroke(undoStackRef.current.length > 0 || hasStroke);
    // If we just popped the last snapshot, the canvas is back to its
    // pre-first-stroke state — clear the "hasStroke" flag.
    if (undoStackRef.current.length === 0) {
      let anyPainted = false;
      const pixels = data.data;
      for (let i = 3; i < pixels.length; i += 4) {
        if (pixels[i] > 0) {
          anyPainted = true;
          break;
        }
      }
      setHasStroke(anyPainted);
    }
  };

  const clear = () => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    snapshot();
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    setHasStroke(false);
  };

  const pointToCanvas = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    const scaleX = canvas.width / rect.width;
    const scaleY = canvas.height / rect.height;
    return {
      x: (e.clientX - rect.left) * scaleX,
      y: (e.clientY - rect.top) * scaleY,
    };
  };

  const drawStrokeSegment = (
    from: { x: number; y: number },
    to: { x: number; y: number },
  ) => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;
    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.lineWidth = brushRef.current;
    if (toolRef.current === "eraser") {
      // Erase only inside the painted layer — no need to touch the
      // base image (it lives behind the canvas, not on it).
      ctx.globalCompositeOperation = "destination-out";
      ctx.strokeStyle = "rgba(0, 0, 0, 1)";
    } else {
      // Replace any prior translucent stroke at this position rather
      // than stacking alpha — without this, dragging back over a
      // freshly-painted region piles up reds and ends up near-opaque,
      // hiding the underlying image and confusing the user about
      // where they've already painted.
      ctx.globalCompositeOperation = "source-over";
      ctx.strokeStyle = STROKE_COLOR;
    }
    ctx.beginPath();
    ctx.moveTo(from.x, from.y);
    ctx.lineTo(to.x, to.y);
    ctx.stroke();
    // Round cap on the segment endpoints alone leaves a tiny gap at
    // very short strokes — add an explicit dot at the destination so a
    // single tap still produces a visible circle.
    ctx.beginPath();
    ctx.arc(to.x, to.y, ctx.lineWidth / 2, 0, Math.PI * 2);
    if (toolRef.current === "eraser") {
      ctx.fillStyle = "rgba(0, 0, 0, 1)";
    } else {
      ctx.fillStyle = STROKE_COLOR;
    }
    ctx.fill();
    ctx.restore();
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0 && e.pointerType === "mouse") return;
    e.preventDefault();
    const canvas = canvasRef.current;
    if (!canvas) return;
    snapshot();
    drawingRef.current = true;
    const p = pointToCanvas(e);
    lastPointRef.current = p;
    drawStrokeSegment(p, p);
    setHasStroke(true);
    canvas.setPointerCapture(e.pointerId);
  };

  const onPointerMove = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!drawingRef.current) return;
    const last = lastPointRef.current;
    const next = pointToCanvas(e);
    if (last) drawStrokeSegment(last, next);
    lastPointRef.current = next;
  };

  const endStroke = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!drawingRef.current) return;
    drawingRef.current = false;
    lastPointRef.current = null;
    const canvas = canvasRef.current;
    if (canvas && canvas.hasPointerCapture(e.pointerId)) {
      canvas.releasePointerCapture(e.pointerId);
    }
  };

  const commit = () => {
    const canvas = canvasRef.current;
    if (!canvas || !naturalSize) return;
    const srcCtx = canvas.getContext("2d");
    if (!srcCtx) return;
    // Threshold to a binary mask: any pixel with non-zero alpha in
    // the painted layer becomes fully white (red channel = 255) on a
    // black background. ComfyUI's LoadImageMask reads the red
    // channel, so partial alpha from the translucent live brush
    // would otherwise produce a soft mask and surprise the user
    // with half-strength edits.
    const src = srcCtx.getImageData(0, 0, canvas.width, canvas.height);
    const out = document.createElement("canvas");
    out.width = canvas.width;
    out.height = canvas.height;
    const ctx = out.getContext("2d");
    if (!ctx) return;
    const dst = ctx.createImageData(canvas.width, canvas.height);
    for (let i = 0; i < src.data.length; i += 4) {
      const painted = src.data[i + 3] > 0;
      const v = painted ? 255 : 0;
      dst.data[i] = v;
      dst.data[i + 1] = v;
      dst.data[i + 2] = v;
      dst.data[i + 3] = 255;
    }
    ctx.putImageData(dst, 0, 0);
    const dataUrl = out.toDataURL("image/png");
    const comma = dataUrl.indexOf(",");
    const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : "";

    // Build the composer-thumbnail preview: base image with the user's
    // painted layer overlaid at full preview alpha. `canvas` itself
    // already holds the translucent red strokes; drawImage'ing it on
    // top of the base picks up that alpha for free.
    const preview = document.createElement("canvas");
    preview.width = canvas.width;
    preview.height = canvas.height;
    const previewCtx = preview.getContext("2d");
    let previewUrl: string;
    if (previewCtx && baseImgRef.current?.complete) {
      previewCtx.drawImage(
        baseImgRef.current,
        0,
        0,
        preview.width,
        preview.height,
      );
      previewCtx.drawImage(canvas, 0, 0);
      previewUrl = preview.toDataURL("image/png");
    } else {
      // Base image wasn't ready — fall back to the binary mask. Better
      // than handing the composer an empty string.
      previewUrl = dataUrl;
    }

    onDone({
      base64,
      preview: previewUrl,
      width: canvas.width,
      height: canvas.height,
    });
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="draw inpaint mask"
      css={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        // New stacking context — keeps anything painted above out of
        // the chat sidebar's z-index 20 layer.
        isolation: "isolate",
        background: "#0b0b0c",
        display: "flex",
        flexDirection: "column",
        paddingTop: "calc(env(safe-area-inset-top, 0px) + 12px)",
        paddingBottom: "calc(env(safe-area-inset-bottom, 0px) + 12px)",
      }}
    >
      <header
        css={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "0 12px 8px",
          color: "#fff",
        }}
      >
        <div css={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            type="button"
            aria-label="cancel"
            onClick={onCancel}
            css={iconButton}
          >
            <span className="material-icons-outlined" css={{ fontSize: 22 }}>
              close
            </span>
          </button>
          <span css={{ ...theme.typography.body2, opacity: 0.7 }}>
            paint where you want changes
          </span>
        </div>
        <button
          type="button"
          disabled={!hasStroke}
          onClick={commit}
          css={{
            ...theme.typography.caption,
            fontFamily: theme.fonts.heading,
            padding: "6px 14px",
            borderRadius: 6,
            border: "none",
            background: hasStroke ? theme.colors.activity.on : "#444",
            color: "#fff",
            cursor: hasStroke ? "pointer" : "not-allowed",
          }}
        >
          done
        </button>
      </header>

      <div
        ref={containerRef}
        css={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          padding: "0 12px",
          position: "relative",
        }}
      >
        <div
          // Wrapper has to carry its own intrinsic dimensions —
          // image + canvas are both position:absolute so they don't
          // contribute to flow sizing, and `width: auto` inside a
          // center-aligned flex column collapses to 0 (the earlier
          // attempt). Take the available width up to 900px and let
          // aspectRatio derive the height; the `maxHeight: 100%`
          // clamp on the outer container reins it in on short
          // viewports without breaking the painted area.
          style={
            naturalSize
              ? { aspectRatio: `${naturalSize.width} / ${naturalSize.height}` }
              : { aspectRatio: "1 / 1" }
          }
          css={{
            position: "relative",
            width: "min(900px, 100%)",
            maxHeight: "100%",
            touchAction: "none",
            background: "#1c1c1f",
            borderRadius: 6,
            overflow: "hidden",
          }}
        >
          <img
            ref={baseImgRef}
            src={imageSrc}
            alt=""
            draggable={false}
            css={{
              position: "absolute",
              inset: 0,
              width: "100%",
              height: "100%",
              objectFit: "contain",
              userSelect: "none",
              pointerEvents: "none",
            }}
          />
          <canvas
            ref={canvasRef}
            aria-label="mask canvas"
            width={naturalSize?.width ?? 512}
            height={naturalSize?.height ?? 512}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={endStroke}
            onPointerCancel={endStroke}
            css={{
              position: "absolute",
              inset: 0,
              width: "100%",
              height: "100%",
              cursor: tool === "eraser" ? "cell" : "crosshair",
              touchAction: "none",
            }}
          />
        </div>
      </div>

      <footer
        css={{
          display: "flex",
          flexWrap: "wrap",
          alignItems: "center",
          gap: 12,
          padding: "8px 12px 0",
          color: "#fff",
        }}
      >
        <div css={{ display: "flex", gap: 4 }}>
          <ToolButton
            label="brush"
            icon="brush"
            active={tool === "brush"}
            onClick={() => setTool("brush")}
          />
          <ToolButton
            label="eraser"
            icon="ink_eraser"
            active={tool === "eraser"}
            onClick={() => setTool("eraser")}
          />
          <ToolButton
            label="undo"
            icon="undo"
            active={false}
            disabled={undoDepth === 0 && !hasStroke}
            onClick={undo}
          />
          <ToolButton
            label="clear"
            icon="delete_outline"
            active={false}
            disabled={!hasStroke}
            onClick={clear}
          />
        </div>
        <label
          css={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            ...theme.typography.caption,
            color: "#fff",
            opacity: 0.85,
          }}
        >
          size
          <input
            type="range"
            min={MIN_BRUSH}
            max={MAX_BRUSH}
            value={brushSize}
            onChange={(e) => setBrushSize(Number(e.target.value))}
            aria-label="brush size"
            css={{ width: 160 }}
          />
          <span
            css={{
              fontFamily:
                "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
              minWidth: 28,
              textAlign: "right",
            }}
          >
            {brushSize}
          </span>
        </label>
      </footer>
    </div>
  );
};

const iconButton = {
  width: 36,
  height: 36,
  borderRadius: "50%",
  border: "none",
  background: "rgba(255,255,255,0.16)",
  color: "#fff",
  cursor: "pointer",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  backdropFilter: "blur(6px)",
  WebkitBackdropFilter: "blur(6px)",
};

const ToolButton = ({
  label,
  icon,
  active,
  disabled,
  onClick,
}: {
  label: string;
  icon: string;
  active: boolean;
  disabled?: boolean;
  onClick: () => void;
}) => (
  <button
    type="button"
    aria-label={label}
    title={label}
    aria-pressed={active}
    disabled={disabled}
    onClick={onClick}
    css={{
      width: 36,
      height: 36,
      borderRadius: 8,
      border: "none",
      background: active ? "rgba(255,255,255,0.22)" : "transparent",
      color: disabled ? "rgba(255,255,255,0.35)" : "#fff",
      cursor: disabled ? "not-allowed" : "pointer",
      display: "flex",
      alignItems: "center",
      justifyContent: "center",
    }}
  >
    <span className="material-icons-outlined" css={{ fontSize: 22 }}>
      {icon}
    </span>
  </button>
);

export default MaskEditor;
