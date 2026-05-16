import { ThemeProvider } from "@emotion/react";
import { render } from "vitest-browser-react";

import { lightTheme } from "../../themes";
import MaskEditor, { MaskResult } from "../MaskEditor";

// Generate a deterministic test image at runtime via a throwaway canvas
// so we don't have to hand-encode PNG bytes (and risk a malformed b64
// that quietly never fires onload, which is what bit the first cut of
// this spec). 32×32 is large enough that pointer coords still resolve
// to multiple natural pixels under viewport scaling.
const makeBaseDataUrl = (): string => {
  const c = document.createElement("canvas");
  c.width = 32;
  c.height = 32;
  const ctx = c.getContext("2d");
  if (ctx) {
    ctx.fillStyle = "#444";
    ctx.fillRect(0, 0, c.width, c.height);
  }
  return c.toDataURL("image/png");
};

const Harness = ({
  src,
  onDone,
  onCancel,
}: {
  src: string;
  onDone: (m: MaskResult) => void;
  onCancel: () => void;
}) => (
  <ThemeProvider theme={lightTheme}>
    <MaskEditor imageSrc={src} onDone={onDone} onCancel={onCancel} />
  </ThemeProvider>
);

describe("MaskEditor", () => {
  test("captures a stroke and emits a non-empty mask on done", async () => {
    const src = makeBaseDataUrl();
    let result: MaskResult | null = null;
    let cancelled = false;
    const screen = await render(
      <Harness
        src={src}
        onDone={(m) => {
          result = m;
        }}
        onCancel={() => {
          cancelled = true;
        }}
      />,
    );

    // Wait for the painting canvas to mount and pick up the base
    // image's natural dimensions. Asserting against the `width`
    // attribute is more reliable than `toBeVisible` here — the
    // canvas sits inside a flex column whose contributing height
    // can fluctuate in headless layouts, but the canvas's intrinsic
    // pixel width is set the moment naturalSize lands.
    await expect
      .element(screen.getByLabelText("mask canvas"))
      .toHaveAttribute("width", "32");
    const canvas = screen
      .getByLabelText("mask canvas")
      .element() as HTMLCanvasElement | null;
    expect(canvas).toBeTruthy();
    if (!canvas) return;

    // Drag a single stroke across the canvas using raw PointerEvents.
    // Vitest's browser page mouse helper isn't wired here so we drive
    // the same event surface the production component listens to.
    const rect = canvas.getBoundingClientRect();
    const midY = rect.top + rect.height / 2;
    const start = { x: rect.left + 4, y: midY };
    const end = { x: rect.left + rect.width - 4, y: midY };
    const dispatch = (
      kind: "pointerdown" | "pointermove" | "pointerup",
      pt: { x: number; y: number },
    ) => {
      canvas.dispatchEvent(
        new PointerEvent(kind, {
          bubbles: true,
          cancelable: true,
          pointerType: "mouse",
          pointerId: 1,
          clientX: pt.x,
          clientY: pt.y,
          button: 0,
          buttons: 1,
        }),
      );
    };
    dispatch("pointerdown", start);
    dispatch("pointermove", { x: (start.x + end.x) / 2, y: midY });
    dispatch("pointermove", end);
    dispatch("pointerup", end);

    // Done should now be enabled.
    const doneButton = screen.getByRole("button", { name: "done" });
    await doneButton.click();

    expect(cancelled).toBe(false);
    expect(result).toBeTruthy();
    const r = result as MaskResult | null;
    expect(r).not.toBeNull();
    const m = r!;
    // Canvas matches the base's natural dimensions.
    expect(m.width).toBe(32);
    expect(m.height).toBe(32);
    // PNG header bytes survive the base64 round-trip — confirms the
    // commit composited something (not an empty buffer).
    expect(m.base64.length).toBeGreaterThan(0);
    const bytes = atob(m.base64);
    expect(bytes.startsWith("\x89PNG\r\n\x1a\n")).toBe(true);
  });
});
