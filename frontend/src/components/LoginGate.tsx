import { ReactNode, useEffect } from "react";
import useSWR from "swr";

import { api, Me } from "../api";

const LoginGate = ({ children }: { children: ReactNode }) => {
  const { data, error, isLoading } = useSWR<Me>("/api/me", api.me, {
    shouldRetryOnError: false,
  });

  const unauthed = !isLoading && (!!error || !data);

  useEffect(() => {
    if (unauthed) {
      const next =
        window.location.pathname +
        window.location.search +
        window.location.hash;
      const target = `/auth/login?next=${encodeURIComponent(next)}`;
      window.location.replace(target);
    }
  }, [unauthed]);

  if (isLoading || unauthed) return null;

  return <>{children}</>;
};

export default LoginGate;
