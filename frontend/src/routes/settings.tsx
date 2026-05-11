/* eslint-disable react-refresh/only-export-components */
import { Theme, useTheme } from "@emotion/react";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import useSWR from "swr";

import { api, Me } from "../api";
import { mq } from "../mq";

const SettingsView = () => {
  const theme = useTheme();
  const navigate = useNavigate();
  const { data: me } = useSWR<Me>("/api/me", api.me);

  return (
    <div
      css={{
        flex: 1,
        overflowY: "auto",
        padding: "60px 24px 32px",
        [mq[0]]: { padding: "60px 16px 24px" },
      }}
    >
      <div
        css={{
          maxWidth: 720,
          margin: "0 auto",
          display: "flex",
          flexDirection: "column",
          gap: 28,
        }}
      >
        <div
          css={{
            display: "flex",
            alignItems: "center",
            gap: 8,
          }}
        >
          <button
            type="button"
            aria-label="back"
            onClick={() => navigate({ to: "/" })}
            css={{
              width: 28,
              height: 28,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              border: "none",
              borderRadius: 4,
              background: "transparent",
              color: theme.colors.text.muted,
              cursor: "pointer",
              "&:hover": {
                background: theme.colors.background.main,
                color: theme.colors.text.main,
              },
            }}
          >
            <span className="material-icons-outlined" css={{ fontSize: 20 }}>
              arrow_back
            </span>
          </button>
          <h1
            css={{
              ...theme.typography.h2,
              color: theme.colors.text.main,
              margin: 0,
            }}
          >
            settings
          </h1>
        </div>

        {me && <AccountSection me={me} theme={theme} />}
      </div>
    </div>
  );
};

const AccountSection = ({ me, theme }: { me: Me; theme: Theme }) => (
  <section css={{ display: "flex", flexDirection: "column", gap: 14 }}>
    <h2
      css={{
        ...theme.typography.h3,
        color: theme.colors.text.main,
        margin: 0,
      }}
    >
      account
    </h2>

    <Row
      label="username"
      theme={theme}
      detail={
        <span
          css={{
            ...theme.typography.body2,
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
            color: theme.colors.text.muted,
          }}
        >
          {me.username}
        </span>
      }
    />

    <div
      css={{
        marginTop: 14,
        paddingTop: 18,
        borderTop: `1px solid ${theme.colors.border}`,
        display: "flex",
        flexDirection: "column",
        gap: 14,
      }}
    >
      <h3
        css={{
          ...theme.typography.body1,
          fontFamily: theme.fonts.heading,
          color: theme.colors.error,
          margin: 0,
        }}
      >
        danger zone
      </h3>
      <DeleteAccountRow me={me} theme={theme} />
    </div>
  </section>
);

const Row = ({
  label,
  detail,
  theme,
  children,
}: {
  label: React.ReactNode;
  detail?: React.ReactNode;
  theme: Theme;
  children?: React.ReactNode;
}) => (
  <div
    css={{
      display: "flex",
      alignItems: "center",
      justifyContent: "space-between",
      gap: 16,
      padding: "12px 0",
      borderTop: `1px solid ${theme.colors.border}`,
      "&:first-of-type": { borderTop: "none" },
    }}
  >
    <div
      css={{
        display: "flex",
        flexDirection: "column",
        gap: 2,
        flex: 1,
        minWidth: 0,
      }}
    >
      <div
        css={{
          ...theme.typography.body1,
          color: theme.colors.text.main,
        }}
      >
        {label}
      </div>
      {detail && <div>{detail}</div>}
    </div>
    {children && <div css={{ flexShrink: 0 }}>{children}</div>}
  </div>
);

const DeleteAccountRow = ({ me, theme }: { me: Me; theme: Theme }) => {
  const [confirming, setConfirming] = useState(false);
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canFire = confirm.trim() === me.username && !busy;

  const handleDelete = async () => {
    if (!canFire) return;
    setBusy(true);
    setError(null);
    try {
      await api.deleteMe();
    } catch (e) {
      setBusy(false);
      setError(String(e));
      return;
    }
    window.location.assign("/");
  };

  const cancel = () => {
    setConfirming(false);
    setConfirm("");
    setError(null);
  };

  return (
    <div css={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <Row
        label="delete account"
        detail={
          <span
            css={{
              ...theme.typography.body2,
              color: theme.colors.text.muted,
            }}
          >
            drops this account and every conversation, message, and attached
            image. cannot be undone.
          </span>
        }
        theme={theme}
      >
        {!confirming && (
          <button
            type="button"
            onClick={() => setConfirming(true)}
            css={dangerButton(theme, false)}
          >
            delete account
          </button>
        )}
      </Row>
      {confirming && (
        <div
          css={{
            display: "flex",
            flexDirection: "column",
            gap: 8,
            padding: "12px 14px",
            border: `1px solid ${theme.colors.error}`,
            borderRadius: theme.border.radius,
            background: theme.colors.background.light,
          }}
        >
          <label
            css={{
              ...theme.typography.caption,
              color: theme.colors.text.muted,
              display: "flex",
              flexDirection: "column",
              gap: 6,
            }}
          >
            type{" "}
            <span
              css={{
                fontFamily: theme.fonts.heading,
                color: theme.colors.text.main,
              }}
            >
              {me.username}
            </span>{" "}
            to confirm
            <input
              type="text"
              autoFocus
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              placeholder={me.username}
              autoComplete="off"
              spellCheck={false}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleDelete();
                else if (e.key === "Escape") cancel();
              }}
              css={{
                padding: "8px 10px",
                borderRadius: 4,
                border: `1px solid ${theme.colors.border}`,
                background: theme.colors.background.main,
                color: theme.colors.text.main,
                fontFamily:
                  "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
                fontSize: 13,
                outline: "none",
                "&:focus": { borderColor: theme.colors.error },
              }}
            />
          </label>
          {error && (
            <div
              css={{
                ...theme.typography.caption,
                color: theme.colors.error,
              }}
            >
              {error}
            </div>
          )}
          <div
            css={{
              display: "flex",
              justifyContent: "flex-end",
              gap: 8,
            }}
          >
            <button
              type="button"
              onClick={cancel}
              disabled={busy}
              css={neutralButton(theme)}
            >
              cancel
            </button>
            <button
              type="button"
              onClick={handleDelete}
              disabled={!canFire}
              css={dangerButton(theme, canFire)}
            >
              {busy ? "deleting…" : "delete"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
};

const neutralButton = (theme: Theme) => ({
  ...theme.typography.body2,
  fontFamily: theme.fonts.heading,
  padding: "6px 12px",
  borderRadius: 4,
  border: `1px solid ${theme.colors.border}`,
  background: "transparent",
  color: theme.colors.text.main,
  cursor: "pointer",
  "&:hover": { background: theme.colors.background.main },
  "&:disabled": { opacity: 0.5, cursor: "default" },
});

const dangerButton = (theme: Theme, active: boolean) => ({
  ...theme.typography.body2,
  fontFamily: theme.fonts.heading,
  padding: "6px 12px",
  borderRadius: 4,
  border: `1px solid ${theme.colors.error}`,
  background: active ? theme.colors.error : "transparent",
  color: active ? "#fff" : theme.colors.error,
  cursor: "pointer",
  transition: "background 120ms ease, color 120ms ease",
  "&:hover": active ? { filter: "brightness(0.95)" } : undefined,
  "&:disabled": { opacity: 0.5, cursor: "default" },
});

export const Route = createFileRoute("/settings")({
  component: SettingsView,
});
