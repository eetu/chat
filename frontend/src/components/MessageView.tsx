import { Theme, useTheme } from "@emotion/react";
import { useEffect, useState } from "react";

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
  /** Live sampler progress, fed from the SSE stream while a ComfyUI
   * img2img job is churning. */
  progress?: { value: number; max: number };
  /** Live preview frame (data URL) from ComfyUI's WebSocket. Only
   * meaningful while the row is pending. */
  previewDataUrl?: string;
  /** Image-mode only: backend is waiting on the image-gen semaphore.
   * Drives the queued state on the pending placeholder. */
  queued?: boolean;
};

type Props = {
  msg: DisplayMessage;
  convId?: string;
  onDeleteFrom?: (id: number) => void;
  onRegenerate?: (id: number) => void;
  /** Click handler that pre-fills the composer with an existing generated
   * image and flips it into img2img mode. Wired only on assistant rows
   * with at least one rendered image. */
  onRemix?: (src: string) => void;
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

/**
 * Image with a skeleton placeholder shown until the actual bytes load.
 * Holds an explicit aspect ratio so the layout doesn't jump when the
 * image swaps in. Most generated images are 1024×1024, so a square
 * default fits well; user uploads usually crop to a square thumbnail.
 */
const ChatImage = ({
  image,
  variant,
  onClick,
}: {
  image: ImageRef;
  variant: "user" | "assistant";
  onClick?: () => void;
}) => {
  const theme = useTheme();
  const [loaded, setLoaded] = useState(false);
  const [errored, setErrored] = useState(false);
  const isUser = variant === "user";
  return (
    <div
      onClick={onClick}
      title={onClick ? "open full size" : undefined}
      css={{
        position: "relative",
        overflow: "hidden",
        borderRadius: theme.border.radius,
        border: `1px solid ${theme.colors.border}`,
        background: theme.colors.background.light,
        cursor: onClick ? "zoom-in" : "default",
        ...(isUser
          ? { width: 220, height: 220 }
          : {
              maxWidth: 480,
              width: "100%",
              aspectRatio: "1 / 1",
            }),
      }}
    >
      {!loaded && !errored && (
        <div
          aria-hidden
          css={{
            position: "absolute",
            inset: 0,
            background: `linear-gradient(90deg, ${theme.colors.background.light} 0%, ${theme.colors.background.main} 50%, ${theme.colors.background.light} 100%)`,
            backgroundSize: "200% 100%",
            animation: "chat-shimmer 1.4s ease-in-out infinite",
            "@keyframes chat-shimmer": {
              "0%": { backgroundPosition: "200% 0" },
              "100%": { backgroundPosition: "-200% 0" },
            },
          }}
        />
      )}
      {errored && (
        <div
          css={{
            position: "absolute",
            inset: 0,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: theme.colors.text.muted,
            ...theme.typography.caption,
          }}
        >
          image unavailable
        </div>
      )}
      <img
        src={image.src}
        alt=""
        loading="lazy"
        onLoad={() => setLoaded(true)}
        onError={() => setErrored(true)}
        css={{
          display: "block",
          width: "100%",
          height: "100%",
          objectFit: isUser ? "cover" : "contain",
          opacity: loaded ? 1 : 0,
          transition: "opacity 180ms ease",
        }}
      />
    </div>
  );
};

/**
 * In-app full-screen viewer. Renders a fixed overlay with the image
 * centered, a close button in the top-left, and ESC + backdrop + button
 * all dismiss. Avoids `window.open` because mobile home-screen PWAs
 * don't have browser chrome, leaving the user stuck on a new tab with
 * no back affordance.
 */
const Lightbox = ({ src, onClose }: { src: string; onClose: () => void }) => {
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    // Lock body scroll so the underlying conversation doesn't move
    // while the viewer is open.
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [onClose]);
  return (
    <div
      role="dialog"
      aria-modal="true"
      onClick={onClose}
      css={{
        position: "fixed",
        inset: 0,
        zIndex: 100,
        background: "rgba(0, 0, 0, 0.92)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 16,
        // Respect notches / status bar on home-screen PWAs.
        paddingTop: "calc(env(safe-area-inset-top, 0px) + 16px)",
        paddingBottom: "calc(env(safe-area-inset-bottom, 0px) + 16px)",
      }}
    >
      <button
        type="button"
        aria-label="close preview"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
        css={{
          position: "absolute",
          top: "calc(env(safe-area-inset-top, 0px) + 12px)",
          left: 12,
          width: 40,
          height: 40,
          borderRadius: "50%",
          border: "none",
          background: "rgba(255, 255, 255, 0.16)",
          color: "#fff",
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          backdropFilter: "blur(6px)",
          WebkitBackdropFilter: "blur(6px)",
        }}
      >
        <span className="material-icons-outlined" css={{ fontSize: 22 }}>
          close
        </span>
      </button>
      <img
        src={src}
        alt=""
        onClick={(e) => e.stopPropagation()}
        css={{
          maxWidth: "100%",
          maxHeight: "100%",
          objectFit: "contain",
          borderRadius: 4,
        }}
      />
    </div>
  );
};

const MessageView = ({
  msg,
  convId,
  onDeleteFrom,
  onRegenerate,
  onRemix,
  busy,
}: Props) => {
  const theme = useTheme();
  const isUser = msg.role === "user";
  const refs = collectImageRefs(msg, convId);
  // Single lightbox source shared across the user and assistant image
  // rows in this message — only one can be open at a time per bubble.
  const [lightboxSrc, setLightboxSrc] = useState<string | null>(null);

  const lightbox = lightboxSrc && (
    <Lightbox src={lightboxSrc} onClose={() => setLightboxSrc(null)} />
  );

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
              <ChatImage
                key={ref.src}
                image={ref}
                variant="user"
                onClick={() => setLightboxSrc(ref.src)}
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
        {lightbox}
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
            <ChatImage
              key={ref.src}
              image={ref}
              variant="assistant"
              onClick={() => setLightboxSrc(ref.src)}
            />
          ))}
        </div>
      )}
      {isPending && !hasImages && (
        <ImageGenPlaceholder
          progress={msg.progress}
          previewDataUrl={msg.previewDataUrl}
          queued={msg.queued}
        />
      )}
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
      {isError && (
        <ErrorActions
          msg={msg}
          onRegenerate={onRegenerate}
          onDeleteFrom={onDeleteFrom}
          busy={busy}
        />
      )}
      {!hasContent && !hasImages && !isPending && !isError && (
        <TypingIndicator />
      )}
      {showActions && (
        <MessageActions
          msg={msg}
          refs={refs}
          onDeleteFrom={onDeleteFrom}
          onRegenerate={onRegenerate}
          onRemix={onRemix}
          busy={busy}
        />
      )}
      {lightbox}
    </div>
  );
};

/// Compact action row shown only on assistant rows whose generation
/// errored — lets the user retry (re-runs the prior user turn) or remove
/// the failed bubble. Hidden on still-pending rows; those use the stop
/// button on the composer.
const ErrorActions = ({
  msg,
  onRegenerate,
  onDeleteFrom,
  busy,
}: {
  msg: DisplayMessage;
  onRegenerate?: (id: number) => void;
  onDeleteFrom?: (id: number) => void;
  busy?: boolean;
}) => {
  const theme = useTheme();
  const canMutate = typeof msg.id === "number" && !busy;
  if (!canMutate) return null;
  return (
    <div
      css={{
        display: "flex",
        gap: 4,
        marginTop: 2,
      }}
    >
      {onRegenerate && (
        <ActionButton
          onClick={() => onRegenerate(msg.id!)}
          label="retry"
          icon="refresh"
          theme={theme}
        />
      )}
      {onDeleteFrom && (
        <ActionButton
          onClick={() => onDeleteFrom(msg.id!)}
          label="remove"
          icon="delete_outline"
          theme={theme}
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
  onRemix,
  busy,
}: {
  msg: DisplayMessage;
  refs: ImageRef[];
  onDeleteFrom?: (id: number) => void;
  onRegenerate?: (id: number) => void;
  onRemix?: (src: string) => void;
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
      {firstImage && onRemix && (
        <ActionButton
          onClick={() => onRemix(firstImage.src)}
          label="remix as new prompt"
          icon="auto_fix_high"
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

const ImageGenPlaceholder = ({
  progress,
  previewDataUrl,
  queued,
}: {
  progress?: { value: number; max: number };
  previewDataUrl?: string;
  queued?: boolean;
}) => {
  const theme = useTheme();
  const stepLabel = queued
    ? "queued — waiting for gpu…"
    : progress && progress.max > 0
      ? `step ${progress.value} / ${progress.max}`
      : "rendering image…";
  const pct =
    progress && progress.max > 0
      ? Math.min(100, Math.round((progress.value / progress.max) * 100))
      : null;
  return (
    <div
      css={{
        position: "relative",
        width: "min(480px, 100%)",
        aspectRatio: "1 / 1",
        borderRadius: theme.border.radius,
        border: `1px dashed ${theme.colors.border}`,
        background: theme.colors.background.light,
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 10,
        color: theme.colors.text.muted,
      }}
    >
      {previewDataUrl ? (
        <img
          src={previewDataUrl}
          alt=""
          css={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            objectFit: "contain",
            // Soften the early latent-decode previews — they're blocky
            // and saturated by nature.
            filter: "saturate(0.85)",
          }}
        />
      ) : (
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
      )}
      <div
        css={{
          position: "absolute",
          left: 0,
          right: 0,
          bottom: 0,
          padding: "10px 12px 12px",
          background: previewDataUrl
            ? "linear-gradient(180deg, rgba(0,0,0,0) 0%, rgba(0,0,0,0.55) 100%)"
            : "transparent",
          color: previewDataUrl ? "#fff" : theme.colors.text.muted,
          display: "flex",
          flexDirection: "column",
          gap: 6,
        }}
      >
        {pct !== null && (
          <div
            aria-hidden
            css={{
              height: 3,
              borderRadius: 2,
              background: previewDataUrl
                ? "rgba(255,255,255,0.25)"
                : theme.colors.background.main,
              overflow: "hidden",
            }}
          >
            <div
              css={{
                height: "100%",
                width: `${pct}%`,
                background: theme.colors.activity.on,
                transition: "width 240ms ease",
              }}
            />
          </div>
        )}
        <div
          css={{
            ...theme.typography.caption,
            textAlign: "center",
            textShadow: previewDataUrl ? "0 1px 2px rgba(0,0,0,0.4)" : "none",
          }}
        >
          {stepLabel}
        </div>
      </div>
    </div>
  );
};

export default MessageView;
