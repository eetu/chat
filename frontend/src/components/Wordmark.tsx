import { useTheme } from "@emotion/react";
import { memo } from "react";

import { mq } from "../mq";

type WordmarkProps = {
  size?: number;
  short?: boolean;
};

/**
 * Brand mark for the chat app. "royale with chat." — pulp fiction reference.
 *
 * Glyph: rounded chat bubble (currentColor stroke) with a warm orange dot
 * inside (`theme.colors.activity.on`). Same accent + tracking as the halo
 * wordmark — sibling-product feel.
 */
const Wordmark = ({ size = 22, short = false }: WordmarkProps) => {
  const theme = useTheme();
  const accent = theme.colors.activity.on;

  return (
    <div
      css={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        color: theme.colors.text.main,
      }}
    >
      <svg
        width={size}
        height={size}
        viewBox="0 0 64 64"
        fill="none"
        aria-hidden="true"
      >
        <path
          d="M12 14h40a6 6 0 0 1 6 6v20a6 6 0 0 1-6 6H30l-9 9v-9h-9a6 6 0 0 1-6-6V20a6 6 0 0 1 6-6z"
          stroke="currentColor"
          strokeWidth="3"
          strokeLinejoin="round"
        />
        <circle cx="32" cy="30" r="5" fill={accent} />
      </svg>
      <span
        css={{
          fontFamily: theme.fonts.body,
          fontWeight: 600,
          letterSpacing: "-0.04em",
          fontSize: size,
          lineHeight: 1,
          whiteSpace: "nowrap",
        }}
      >
        {short ? null : (
          <span css={{ [mq[0]]: { display: "none" } }}>royale with </span>
        )}
        chat<span css={{ color: accent }}>.</span>
      </span>
    </div>
  );
};

export default memo(Wordmark);
