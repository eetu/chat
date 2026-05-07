import { useTheme } from "@emotion/react";

import { Message } from "../api";
import Markdown from "./Markdown";
import TypingIndicator from "./TypingIndicator";

type DisplayMessage = Pick<Message, "role" | "content"> & {
  id?: number;
  images?: string[];
  status?: "done" | "pending" | "error";
};

const imgSrc = (s: string) =>
  s.startsWith("data:") ? s : `data:image/*;base64,${s}`;

const MessageView = ({ msg }: { msg: DisplayMessage }) => {
  const theme = useTheme();
  const isUser = msg.role === "user";

  if (isUser) {
    return (
      <div
        css={{
          display: "flex",
          flexDirection: "column",
          alignItems: "flex-end",
          marginBottom: 18,
          gap: 6,
        }}
      >
        {msg.images && msg.images.length > 0 && (
          <div
            css={{
              display: "flex",
              gap: 6,
              flexWrap: "wrap",
              justifyContent: "flex-end",
              maxWidth: "78%",
            }}
          >
            {msg.images.map((src) => (
              <img
                key={src}
                src={imgSrc(src)}
                alt=""
                loading="lazy"
                css={{
                  maxWidth: 220,
                  maxHeight: 220,
                  borderRadius: theme.border.radius,
                  border: `1px solid ${theme.colors.border}`,
                  objectFit: "cover",
                }}
              />
            ))}
          </div>
        )}
        {msg.content && (
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
      </div>
    );
  }

  const hasImages = !!msg.images && msg.images.length > 0;
  const hasContent = !!msg.content;
  const isPending = msg.status === "pending";
  const isError = msg.status === "error";

  return (
    <div
      css={{
        marginBottom: 22,
        wordBreak: "break-word",
        color: theme.colors.text.main,
        display: "flex",
        flexDirection: "column",
        gap: 8,
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
          {msg.images!.map((src) => (
            <img
              key={src}
              src={imgSrc(src)}
              alt=""
              loading="lazy"
              css={{
                maxWidth: 480,
                maxHeight: 480,
                width: "100%",
                height: "auto",
                borderRadius: theme.border.radius,
                border: `1px solid ${theme.colors.border}`,
                objectFit: "contain",
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
          <div
            css={{
              ...theme.typography.caption,
              color: theme.colors.text.muted,
              fontStyle: "italic",
              maxWidth: 480,
            }}
          >
            {msg.content}
          </div>
        ) : (
          <Markdown>{msg.content}</Markdown>
        ))}
      {!hasContent && !hasImages && !isPending && !isError && (
        <TypingIndicator />
      )}
    </div>
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
