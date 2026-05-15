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
        chatCap={false}
        imageGen={true}
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
});
