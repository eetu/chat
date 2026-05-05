import { useNavigate } from "@tanstack/react-router";
import { ReactNode, useEffect } from "react";
import useSWR from "swr";

import { api, Me } from "../api";

const LoginGate = ({ children }: { children: ReactNode }) => {
  const navigate = useNavigate();
  const { data, error, isLoading } = useSWR<Me>("/api/me", api.me, {
    shouldRetryOnError: false,
  });

  const unauthed = !isLoading && (!!error || !data);

  useEffect(() => {
    if (unauthed) {
      navigate({ href: "/auth/login", replace: true });
    }
  }, [unauthed, navigate]);

  if (isLoading || unauthed) return null;

  return <>{children}</>;
};

export default LoginGate;
