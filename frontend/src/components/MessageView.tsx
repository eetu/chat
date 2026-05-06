import { useTheme } from "@emotion/react";

import { Message } from "../api";
import Markdown from "./Markdown";
import TypingIndicator from "./TypingIndicator";

type DisplayMessage = Pick<Message, "role" | "content"> & {
  id?: number;
  images?: string[];
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
      {hasContent && <Markdown>{msg.content}</Markdown>}
      {!hasContent && !hasImages && <TypingIndicator />}
    </div>
  );
};

export default MessageView;
