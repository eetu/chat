import { ThemeProvider } from "@emotion/react";
import { useRef } from "react";
import { render } from "vitest-browser-react";

import { lightTheme } from "../../themes";
import Composer, { ComposerHandle } from "../Composer";

// 1×1 transparent PNG — small enough to inline and recognisable when it
// flows back through `onSend`'s `images` array.
const SEED_PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
const SEED_DATA_URL = `data:image/png;base64,${SEED_PNG_B64}`;

type SendArgs = Parameters<React.ComponentProps<typeof Composer>["onSend"]>;

const Harness = ({
  onSendSpy,
  seedId = "seed-1",
}: {
  onSendSpy: (...args: SendArgs) => void;
  seedId?: string;
}) => {
  // Don't actually need the handle for the test, but typing it tracks
  // what the production route passes so the test catches signature
  // drift if `Composer` adds new required props.
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
        suggestedSeed={{ id: seedId, dataUrl: SEED_DATA_URL }}
      />
    </ThemeProvider>
  );
};

describe("img2img chain seed", () => {
  beforeEach(() => {
    try {
      window.localStorage.removeItem("chat:attachmentMode");
      window.localStorage.removeItem("chat:img2img");
    } catch {
      // ignore
    }
  });

  test("auto-attaches the suggested seed and sends it as an image", async () => {
    const sends: SendArgs[] = [];
    const screen = await render(
      <Harness
        onSendSpy={(...args) => {
          sends.push(args);
        }}
      />,
    );

    // The img2img chip is the user-facing tell that the chain consumed
    // the seed: it only renders when (attached.length > 0 && img2img).
    await expect.element(screen.getByText(/img2img/i)).toBeVisible();

    // Compose a follow-up prompt and submit. The thumbnail chip and the
    // base64 should flow through `onSend` so the caller can post them to
    // /api/chat with mode="image".
    const textarea = screen.getByPlaceholder("message");
    await textarea.fill("paint it blue");
    await screen.getByLabelText("send").click();

    // One submission, mode=image, the seed base64 carried as the only image.
    expect(sends).toHaveLength(1);
    const [content, images, mode] = sends[0];
    expect(content).toBe("paint it blue");
    expect(mode).toBe("image");
    expect(images).toEqual([SEED_PNG_B64]);
  });

  test("chain after a prior inpaint session lands in edit mode, not inpaint", async () => {
    // Simulate a leftover preference from a previous inpaint turn —
    // the next image-gen chain shouldn't silently re-enter inpaint
    // since the freshly-generated image has no mask drawn yet.
    // Confirm the suggested-seed handler downshifts to "edit" by
    // checking the absence of a mask payload + the sub_mode falling
    // through to undefined (so the backend infers img2img).
    window.localStorage.setItem("chat:attachmentMode", "inpaint");
    const sends: SendArgs[] = [];
    const screen = await render(
      <Harness
        onSendSpy={(...args) => {
          sends.push(args);
        }}
      />,
    );

    await expect.element(screen.getByText(/img2img/i)).toBeVisible();

    const textarea = screen.getByPlaceholder("message");
    await textarea.fill("zoom out");
    await screen.getByLabelText("send").click();

    expect(sends).toHaveLength(1);
    const [, , mode, , , subMode, mask] = sends[0];
    expect(mode).toBe("image");
    // Chain forces "edit" regardless of stored attachmentMode —
    // sub_mode is "img2img" (or undefined; backend defaults to it).
    expect(subMode === "img2img" || subMode === undefined).toBe(true);
    expect(mask).toBeUndefined();
  });
});
