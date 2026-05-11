import { useTheme } from "@emotion/react";
import useSWR from "swr";

import { api } from "../api";

/**
 * Thin top-of-main banner shown when `/status.upstream` is false. The
 * status endpoint pings Ollama on each call, so this surfaces an
 * unreachable upstream before the user finds out by sending a message
 * that fails mid-stream.
 */
const HealthBanner = () => {
  const theme = useTheme();
  const { data } = useSWR("/status", api.status, {
    refreshInterval: 20_000,
    revalidateOnFocus: true,
    shouldRetryOnError: false,
  });
  if (!data || data.upstream) return null;
  return (
    <div
      role="status"
      css={{
        ...theme.typography.caption,
        fontFamily: theme.fonts.heading,
        borderBottom: `1px solid ${theme.colors.error}`,
        background: theme.colors.background.light,
        color: theme.colors.error,
        padding: "8px 56px",
        textAlign: "center",
      }}
    >
      ollama upstream unreachable — sending will fail until it&apos;s back
    </div>
  );
};

export default HealthBanner;
