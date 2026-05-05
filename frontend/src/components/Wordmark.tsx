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
          d="M20 11H44a10 10 0 0 1 10 10V33a10 10 0 0 1-10 10H30l-10 10v-10a10 10 0 0 1-10-10V21a10 10 0 0 1 10-10z"
          stroke="currentColor"
          strokeWidth="3.5"
          strokeLinejoin="round"
        />
        <circle cx="32" cy="27" r="6" fill={accent} />
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
