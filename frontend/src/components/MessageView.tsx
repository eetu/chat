import { Theme, useTheme } from "@emotion/react";
import { useState } from "react";

import { imageUrl, Message } from "../api";
import Markdown from "./Markdown";
import TypingIndicator from "./TypingIndicator";

type DisplayMessage = Pick<Message, "role" | "content"> & {
  id?: number;
  /** Optimistic-state base64 for messages not yet persisted. */
  images?: string[];
  /** Number of persisted attachments — load via `imageUrl(...)`. */
  image_count?: number;
  status?: "done" | "pending" | "error";
};

type Props = {
  msg: DisplayMessage;
  convId?: string;
  onDeleteFrom?: (id: number) => void;
  onRegenerate?: (id: number) => void;
  busy?: boolean;
};

// Optimistic data URL for pre-persistence base64. Concrete MIME so
// macOS clipboard accepts the Blob — `image/*` wildcards leave
// `blob.type` empty and the ClipboardItem silently fails.
const pngDataUrl = (s: string) =>
  s.startsWith("data:") ? s : `data:image/png;base64,${s}`;

type ImageRef = { src: string; isUrl: boolean };

const collectImageRefs = (msg: DisplayMessage, convId?: string): ImageRef[] => {
  if (msg.images && msg.images.length > 0) {
    return msg.images.map((s) => ({ src: pngDataUrl(s), isUrl: false }));
  }
  if (
    convId &&
    typeof msg.id === "number" &&
    msg.image_count &&
    msg.image_count > 0
  ) {
    return Array.from({ length: msg.image_count }, (_, idx) => ({
      src: imageUrl(convId, msg.id!, idx),
      isUrl: true,
    }));
  }
  return [];
};

// Chrome blocks top-level `data:` navigation, so opening a persisted
// image just delegates to a same-origin URL. Optimistic (pre-persist)
// images route through a blob URL.
const openImageFullSize = async (ref: ImageRef) => {
  if (ref.isUrl) {
    window.open(ref.src, "_blank", "noopener,noreferrer");
    return;
  }
  try {
    const blob = await (await fetch(ref.src)).blob();
    const url = URL.createObjectURL(blob);
    window.open(url, "_blank", "noopener,noreferrer");
    window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
  } catch (e) {
    console.error("open image failed", e);
  }
};

const MessageView = ({
  msg,
  convId,
  onDeleteFrom,
  onRegenerate,
  busy,
}: Props) => {
  const theme = useTheme();
  const isUser = msg.role === "user";
  const refs = collectImageRefs(msg, convId);

  if (isUser) {
    const userHasContent = !!msg.content;
    const userHasImages = refs.length > 0;
    const userShowActions = userHasContent || userHasImages;
    return (
      <div
        css={{
          display: "flex",
          flexDirection: "column",
          alignItems: "flex-end",
          marginBottom: 18,
          gap: 6,
          "& .message-actions": {
            opacity: 0,
            transition: "opacity 120ms ease",
          },
          "&:hover .message-actions, &:focus-within .message-actions": {
            opacity: 1,
          },
          "@media (hover: none)": {
            "& .message-actions": { opacity: 1 },
          },
        }}
      >
        {userHasImages && (
          <div
            css={{
              display: "flex",
              gap: 6,
              flexWrap: "wrap",
              justifyContent: "flex-end",
              maxWidth: "78%",
            }}
          >
            {refs.map((ref) => (
              <img
                key={ref.src}
                src={ref.src}
                alt=""
                loading="lazy"
                onClick={() => void openImageFullSize(ref)}
                css={{
                  maxWidth: 220,
                  maxHeight: 220,
                  borderRadius: theme.border.radius,
                  border: `1px solid ${theme.colors.border}`,
                  objectFit: "cover",
                  cursor: "zoom-in",
                }}
              />
            ))}
          </div>
        )}
        {userHasContent && (
          <div
            css={{
              maxWidth: "78%",
              padding: "10px 14px",
              borderRadius: theme.border.radius,
              background: theme.colors.activity.onSoft,
              color: theme.colors.text.main,
              wordBreak: "break-word",
              whiteSpace: "pre-wrap",
              ...theme.typography.body1,
            }}
          >
            {msg.content}
          </div>
        )}
        {userShowActions && (
          <MessageActions
            msg={msg}
            refs={refs}
            onDeleteFrom={onDeleteFrom}
            busy={busy}
          />
        )}
      </div>
    );
  }

  const hasImages = refs.length > 0;
  const hasContent = !!msg.content;
  const isPending = msg.status === "pending";
  const isError = msg.status === "error";

  const showActions = !isPending && !isError && (hasContent || hasImages);

  return (
    <div
      css={{
        marginBottom: 22,
        wordBreak: "break-word",
        color: theme.colors.text.main,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        "& .message-actions": {
          opacity: 0,
          transition: "opacity 120ms ease",
        },
        "&:hover .message-actions, &:focus-within .message-actions": {
          opacity: 1,
        },
        "@media (hover: none)": {
          "& .message-actions": { opacity: 1 },
        },
      }}
    >
      {hasImages && (
        <div
          css={{
            display: "flex",
            gap: 6,
            flexWrap: "wrap",
          }}
        >
          {refs.map((ref) => (
            <img
              key={ref.src}
              src={ref.src}
              alt=""
              loading="lazy"
              title="open full size"
              onClick={() => void openImageFullSize(ref)}
              css={{
                maxWidth: 480,
                maxHeight: 480,
                width: "100%",
                height: "auto",
                borderRadius: theme.border.radius,
                border: `1px solid ${theme.colors.border}`,
                objectFit: "contain",
                cursor: "zoom-in",
              }}
            />
          ))}
        </div>
      )}
      {isPending && !hasImages && <ImageGenPlaceholder />}
      {isError && !hasContent && (
        <div
          css={{
            ...theme.typography.caption,
            color: theme.colors.error,
            fontStyle: "italic",
          }}
        >
          generation failed
        </div>
      )}
      {hasContent &&
        (isError ? (
          <div
            css={{
              ...theme.typography.caption,
              color: theme.colors.error,
              fontStyle: "italic",
            }}
          >
            {msg.content}
          </div>
        ) : hasImages ? (
          <CollapsibleCaption text={msg.content} />
        ) : (
          <Markdown>{msg.content}</Markdown>
        ))}
      {!hasContent && !hasImages && !isPending && !isError && (
        <TypingIndicator />
      )}
      {showActions && (
        <MessageActions
          msg={msg}
          refs={refs}
          onDeleteFrom={onDeleteFrom}
          onRegenerate={onRegenerate}
          busy={busy}
        />
      )}
    </div>
  );
};

const MessageActions = ({
  msg,
  refs,
  onDeleteFrom,
  onRegenerate,
  busy,
}: {
  msg: DisplayMessage;
  refs: ImageRef[];
  onDeleteFrom?: (id: number) => void;
  onRegenerate?: (id: number) => void;
  busy?: boolean;
}) => {
  const theme = useTheme();
  const [flash, setFlash] = useState(false);
  const isAssistant = msg.role === "assistant";
  // Image-side actions only apply to assistant messages — user uploads
  // came from the user's own device, no point copying or re-downloading.
  const firstImage = isAssistant ? refs[0] : undefined;
  const canMutate = typeof msg.id === "number" && !busy;

  const copy = async () => {
    try {
      if (firstImage) {
        const raw = await (await fetch(firstImage.src)).blob();
        const blob = new Blob([raw], { type: "image/png" });
        await navigator.clipboard.write([
          new ClipboardItem({ "image/png": blob }),
        ]);
      } else if (msg.content) {
        await navigator.clipboard.writeText(msg.content);
      } else {
        return;
      }
      setFlash(true);
      window.setTimeout(() => setFlash(false), 1200);
    } catch (e) {
      console.error("copy failed", e);
    }
  };

  const downloadImage = () => {
    if (!firstImage) return;
    const a = document.createElement("a");
    a.href = firstImage.src;
    a.download = `chat-image-${Date.now()}.png`;
    document.body.appendChild(a);
    a.click();
    a.remove();
  };

  return (
    <div
      className="message-actions"
      css={{
        display: "flex",
        gap: 4,
        marginTop: 2,
      }}
    >
      <ActionButton
        onClick={copy}
        label={flash ? "copied" : firstImage ? "copy image" : "copy text"}
        icon={flash ? "check" : "content_copy"}
        theme={theme}
      />
      {firstImage && (
        <ActionButton
          onClick={downloadImage}
          label="download image"
          icon="download"
          theme={theme}
        />
      )}
      {onRegenerate && canMutate && msg.role === "assistant" && (
        <ActionButton
          onClick={() => onRegenerate(msg.id!)}
          label="regenerate"
          icon="refresh"
          theme={theme}
        />
      )}
      {onDeleteFrom && canMutate && (
        <ActionButton
          onClick={() => onDeleteFrom(msg.id!)}
          label="delete from here"
          icon="delete_outline"
          theme={theme}
        />
      )}
    </div>
  );
};

type ActionButtonProps = {
  onClick: () => void | Promise<void>;
  label: string;
  icon: string;
  theme: Theme;
};

const ActionButton = ({ onClick, label, icon, theme }: ActionButtonProps) => (
  <button
    type="button"
    aria-label={label}
    title={label}
    onClick={() => void onClick()}
    css={{
      width: 28,
      height: 28,
      display: "flex",
      alignItems: "center",
      justifyContent: "center",
      border: "none",
      borderRadius: 6,
      background: "transparent",
      color: theme.colors.text.muted,
      cursor: "pointer",
      "&:hover": {
        background: theme.colors.background.light,
        color: theme.colors.text.main,
      },
    }}
  >
    <span className="material-icons-outlined" css={{ fontSize: 18 }}>
      {icon}
    </span>
  </button>
);

const CollapsibleCaption = ({ text }: { text: string }) => {
  const theme = useTheme();
  const [open, setOpen] = useState(false);
  return (
    <details
      open={open}
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
      css={{
        maxWidth: 480,
        ...theme.typography.caption,
        color: theme.colors.text.muted,
      }}
    >
      <summary
        css={{
          cursor: "pointer",
          display: "inline-flex",
          alignItems: "center",
          gap: 4,
          color: theme.colors.text.muted,
          listStyle: "none",
          "&::-webkit-details-marker": { display: "none" },
          "&:hover": { color: theme.colors.text.main },
        }}
      >
        <span
          className="material-icons-outlined"
          css={{
            fontSize: 16,
            transition: "transform 120ms ease",
            transform: open ? "rotate(90deg)" : "rotate(0deg)",
          }}
        >
          chevron_right
        </span>
        {open ? "hide prompt" : "show prompt"}
      </summary>
      <div css={{ marginTop: 4, fontStyle: "italic" }}>{text}</div>
    </details>
  );
};

const ImageGenPlaceholder = () => {
  const theme = useTheme();
  return (
    <div
      css={{
        width: "min(480px, 100%)",
        aspectRatio: "1 / 1",
        borderRadius: theme.border.radius,
        border: `1px dashed ${theme.colors.border}`,
        background: theme.colors.background.light,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 10,
        color: theme.colors.text.muted,
      }}
    >
      <span
        className="material-icons-outlined"
        css={{
          fontSize: 36,
          animation: "chat-pulse 1.6s ease-in-out infinite",
          "@keyframes chat-pulse": {
            "0%, 100%": { opacity: 0.35 },
            "50%": { opacity: 1 },
          },
        }}
      >
        image
      </span>
      <div css={{ ...theme.typography.caption }}>rendering image…</div>
    </div>
  );
};

export default MessageView;
