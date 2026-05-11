import { useTheme } from "@emotion/react";
import { PointerEvent, ReactNode, useRef, useState } from "react";

const REVEAL_PX = 88;
const TRIGGER_PX = 200;

/**
 * Touch-only swipe-to-delete with a hover-revealed × button for mouse /
 * trackpad users. Trackpad two-finger pans were getting interpreted as
 * swipes; gating on `pointerType === "touch"` fixes that.
 *
 * - Touch: drag left up to TRIGGER_PX. Past threshold = commit. Between
 *   REVEAL_PX and TRIGGER_PX = stay open. Otherwise snap closed.
 * - Mouse / pen: small × button appears on row hover; click → confirm dialog.
 */
const SwipeRow = ({
  children,
  onDelete,
  confirmLabel = "delete this conversation?",
  hideMouseDelete = false,
}: {
  children: ReactNode;
  onDelete: () => void;
  confirmLabel?: string;
  /** When true, suppress the hover-revealed × button so a wrapping
   * component can render its own action menu (kebab, etc.) without two
   * affordances overlapping. Touch swipe-to-delete is unaffected. */
  hideMouseDelete?: boolean;
}) => {
  const theme = useTheme();
  const [offset, setOffset] = useState(0);
  const [dragging, setDragging] = useState(false);
  const startXRef = useRef<number | null>(null);
  const startOffsetRef = useRef(0);

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.pointerType !== "touch") return;
    startXRef.current = e.clientX;
    startOffsetRef.current = offset;
    setDragging(true);
    (e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (e.pointerType !== "touch" || startXRef.current == null) return;
    const delta = e.clientX - startXRef.current;
    const next = Math.min(
      0,
      Math.max(-TRIGGER_PX - 40, startOffsetRef.current + delta),
    );
    setOffset(next);
  };

  const finish = (e: PointerEvent<HTMLDivElement>) => {
    startXRef.current = null;
    setDragging(false);
    // Release explicitly: iOS Safari can stall subsequent touch / scroll on
    // unrelated elements if a captured pointer's element is unmounted (which
    // happens when the swipe past TRIGGER_PX deletes this row) before the
    // capture is released.
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // ignore — capture may already be released
    }
    if (offset <= -TRIGGER_PX) {
      setOffset(0);
      // Defer the unmount-causing delete to the next frame so the pointer
      // event lifecycle finishes cleanly before the row is removed.
      requestAnimationFrame(() => onDelete());
    } else if (offset <= -REVEAL_PX) {
      setOffset(-REVEAL_PX);
    } else {
      setOffset(0);
    }
  };

  const onMouseDelete = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (window.confirm(confirmLabel)) onDelete();
  };

  return (
    <div
      className="swipe-row"
      css={{
        position: "relative",
        overflow: "hidden",
        userSelect: "none",
        touchAction: "pan-y",
        "@media (hover: hover)": {
          "&:hover .swipe-row__x": { opacity: 1, pointerEvents: "auto" },
        },
      }}
    >
      {/* swipe action (touch) — full-bleed red panel behind the row, with
          the click target anchored to the right so the same delete colour
          fills however far the user has dragged. */}
      <div
        css={{
          position: "absolute",
          inset: 0,
          background: theme.colors.error,
        }}
      />
      <button
        type="button"
        onClick={() => {
          onDelete();
          setOffset(0);
        }}
        tabIndex={-1}
        css={{
          position: "absolute",
          top: 0,
          right: 0,
          bottom: 0,
          width: REVEAL_PX,
          border: "none",
          background: "transparent",
          color: "#fff",
          fontFamily: theme.fonts.heading,
          fontSize: 13,
          cursor: "pointer",
        }}
      >
        delete
      </button>
      <div
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={finish}
        onPointerCancel={finish}
        css={{
          position: "relative",
          transform: `translateX(${offset}px)`,
          transition: dragging ? "none" : "transform 150ms ease",
          background: theme.colors.background.light,
        }}
      >
        {children}
        {!hideMouseDelete && (
          /* hover-revealed × button (mouse / pen) */
          <button
            type="button"
            aria-label="delete"
            className="swipe-row__x"
            onClick={onMouseDelete}
            css={{
              position: "absolute",
              top: "50%",
              right: 8,
              transform: "translateY(-50%)",
              width: 24,
              height: 24,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              border: "none",
              borderRadius: 4,
              background: "transparent",
              color: theme.colors.text.muted,
              cursor: "pointer",
              opacity: 0,
              pointerEvents: "none",
              transition: "opacity 120ms ease, background 120ms ease",
              "@media (hover: hover)": {
                "&:hover": {
                  background: theme.colors.text.light,
                  color: theme.colors.error,
                },
              },
            }}
          >
            <span className="material-icons-outlined" css={{ fontSize: 18 }}>
              close
            </span>
          </button>
        )}
      </div>
    </div>
  );
};

export default SwipeRow;
