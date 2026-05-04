import { keyframes, useTheme } from "@emotion/react";
import { memo } from "react";

const pulse = keyframes`
  0%, 60%, 100% { opacity: 0.25; transform: scale(0.85); }
  30% { opacity: 1; transform: scale(1); }
`;

/**
 * Three pulsing orange dots, staggered. Borrows the halo design system's
 * breathing-accent vocabulary: warm centre on a calm grey field, the only
 * saturated colour on screen. Used while waiting for the first delta on a
 * larger model.
 */
const TypingIndicator = () => {
  const theme = useTheme();
  const accent = theme.colors.activity.on;

  return (
    <div
      role="status"
      aria-label="waiting for response"
      css={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "4px 0",
      }}
    >
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          css={{
            width: 7,
            height: 7,
            borderRadius: "50%",
            background: accent,
            display: "inline-block",
            animation: `${pulse} 1.2s ease-in-out ${i * 0.18}s infinite`,
          }}
        />
      ))}
    </div>
  );
};

export default memo(TypingIndicator);
