import { useTheme } from "@emotion/react";
import { ReactNode } from "react";
import useSWR from "swr";

import { api, Me } from "../api";
import Wordmark from "./Wordmark";

const LoginGate = ({ children }: { children: ReactNode }) => {
  const theme = useTheme();
  const { data, error, isLoading } = useSWR<Me>("/api/me", api.me, {
    shouldRetryOnError: false,
  });

  if (isLoading) return null;

  if (error || !data) {
    return (
      <div
        css={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          gap: 24,
        }}
      >
        <Wordmark size={28} />
        <p
          css={{
            ...theme.typography.body2,
            color: theme.colors.text.muted,
            maxWidth: 320,
            textAlign: "center",
          }}
        >
          you brought a knife to a gunfight. sign in first.
        </p>
        <a
          href="/auth/login"
          css={{
            padding: "10px 18px",
            borderRadius: theme.border.radius,
            background: theme.colors.activity.on,
            color: "#fff",
            fontFamily: theme.fonts.heading,
            fontSize: 14,
          }}
        >
          sign in
        </a>
      </div>
    );
  }

  return <>{children}</>;
};

export default LoginGate;
