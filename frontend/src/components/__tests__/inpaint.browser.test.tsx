import { ThemeProvider } from "@emotion/react";
import { useRef } from "react";
import { render } from "vitest-browser-react";

import { lightTheme } from "../../themes";
import Composer, { ComposerHandle, ComposerSend } from "../Composer";

const SEED_PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
const SEED_DATA_URL = `data:image/png;base64,${SEED_PNG_B64}`;

const Harness = ({
  onSendSpy,
}: {
  onSendSpy: (payload: ComposerSend) => void;
}) => {
  const ref = useRef<ComposerHandle>(null);
  return (
    <ThemeProvider theme={lightTheme}>
      <Composer
        ref={ref}
        onSend={onSendSpy}
        history={[]}
        model="qwen3-image"
        onModelChange={() => {}}
        vision={false}
        refinerAvailable={false}
        img2imgAvailable={true}
        voiceInAvailable={false}
        personas={[]}
        suggestedSeed={{ id: "seed-inpaint", dataUrl: SEED_DATA_URL }}
      />
    </ThemeProvider>
  );
};

describe("inpaint flow", () => {
  beforeEach(() => {
    // Wipe the attachment-mode preference so each test starts from
    // the same baseline regardless of earlier runs in the same
    // browser context.
    try {
      window.localStorage.removeItem("chat:attachmentMode");
      window.localStorage.removeItem("chat:img2img");
    } catch {
      // ignore
    }
  });

  test("switches into inpaint, captures a mask, and sends sub_mode=inpaint", async () => {
    const sends: ComposerSend[] = [];
    const screen = await render(
      <Harness onSendSpy={(payload) => sends.push(payload)} />,
    );

    // Wait for the chain seed to auto-attach. Once it does, the
    // remove-attachment X button appears on the thumbnail and the
    // mode pill renders.
    await expect
      .element(screen.getByLabelText("remove attachment"))
      .toBeVisible();

    // Pick the inpaint segment in the attachment-mode pill. Single
    // attachment + img2imgAvailable=true is exactly the case where
    // the inpaint option is offered.
    await screen.getByRole("radio", { name: "inpaint mask" }).click();

    // The draw-mask button only appears once inpaint is active.
    const drawBtn = screen.getByLabelText("draw mask");
    await expect.element(drawBtn).toBeVisible();
    await drawBtn.click();

    // Wait for the editor canvas to size itself to the 1×1 seed image
    // before we paint. width="1" is fine for the test — we just need
    // a non-zero canvas to exercise the stroke path.
    const canvasLocator = screen.getByLabelText("mask canvas");
    await expect.element(canvasLocator).toHaveAttribute("width", "1");
    const canvas = canvasLocator.element() as HTMLCanvasElement;
    const rect = canvas.getBoundingClientRect();
    const x = rect.left + rect.width / 2;
    const y = rect.top + rect.height / 2;
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
    dispatch("pointerdown", { x, y });
    dispatch("pointermove", { x: x + 1, y });
    dispatch("pointerup", { x: x + 1, y });

    // Commit the mask. The editor's done button is initially
    // disabled until a stroke lands, which the dispatches above
    // satisfy.
    await screen.getByRole("button", { name: "done" }).click();

    // Now write a prompt and submit.
    await screen.getByPlaceholder("message").fill("replace with a fish");
    await screen.getByLabelText("send").click();

    expect(sends).toHaveLength(1);
    const [{ content, images, mode, subMode, mask }] = sends;
    expect(content).toBe("replace with a fish");
    expect(images).toEqual([SEED_PNG_B64]);
    expect(mode).toBe("image");
    expect(subMode).toBe("inpaint");
    // Mask payload is a base64 PNG produced by the commit pipeline.
    expect(typeof mask).toBe("string");
    expect((mask as string).length).toBeGreaterThan(0);
    const decoded = atob(mask as string);
    expect(decoded.startsWith("\x89PNG\r\n\x1a\n")).toBe(true);
  }, 20_000);
});
