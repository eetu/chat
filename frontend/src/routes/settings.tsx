/* eslint-disable react-refresh/only-export-components */
import { Theme, useTheme } from "@emotion/react";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useMemo, useRef, useState } from "react";
import useSWR from "swr";

import { api, Document, Me, Status } from "../api";
import { mq } from "../mq";
import { ThemeOverride, useThemeOverride } from "../theme";
import {
  normalizeVoices,
  readSttLangPref,
  readVoiceOverride,
  SttLangPref,
  SUPPORTED_STT_LANGS,
  writeSttLangPref,
  writeVoiceOverride,
} from "../tts";

const RAG_MODEL_KEY = "chat:ragModel";

const SettingsView = () => {
  const theme = useTheme();
  const navigate = useNavigate();
  const { data: me } = useSWR<Me>("/api/me", api.me);
  const { data: status } = useSWR<Status>("/status", api.status);
  const ttsAvailable = status?.voice_out_available ?? false;
  const sttAvailable = status?.voice_in_available ?? false;
  const ragAvailable = status?.rag_available ?? false;

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
        <AppearanceSection theme={theme} />
        {(ttsAvailable || sttAvailable) && (
          <VoiceSection
            theme={theme}
            ttsAvailable={ttsAvailable}
            sttAvailable={sttAvailable}
          />
        )}
        {ragAvailable && <DocumentsSection theme={theme} />}
      </div>
    </div>
  );
};

const DocumentsSection = ({ theme }: { theme: Theme }) => {
  const { data: docs, mutate } = useSWR<Document[]>(
    "/api/documents",
    api.listDocuments,
  );
  const { data: modelsData } = useSWR("/api/embedding-models", () =>
    api.embeddingModels(),
  );
  const models = modelsData?.models ?? [];
  const fileRef = useRef<HTMLInputElement>(null);
  const [model, setModel] = useState<string>(() => {
    try {
      return window.localStorage.getItem(RAG_MODEL_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Default to the first detected model when nothing is stored yet.
  if (!model && models.length > 0) {
    setModel(models[0]);
    try {
      window.localStorage.setItem(RAG_MODEL_KEY, models[0]);
    } catch {
      // ignore
    }
  }

  const onModelChange = (next: string) => {
    setModel(next);
    try {
      window.localStorage.setItem(RAG_MODEL_KEY, next);
    } catch {
      // ignore
    }
  };

  const onPickFile = async (file: File) => {
    setBusy(true);
    setError(null);
    try {
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result));
        r.onerror = () => reject(r.error);
        r.readAsDataURL(file);
      });
      const comma = dataUrl.indexOf(",");
      const b64 = comma >= 0 ? dataUrl.slice(comma + 1) : "";
      if (!b64) throw new Error("could not read file");
      await api.uploadDocument({
        name: file.name,
        content_b64: b64,
        mime: file.type || undefined,
        model: model || undefined,
      });
      await mutate();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const onDelete = async (id: number) => {
    if (!window.confirm("delete this document?")) return;
    try {
      await api.deleteDocument(id);
      await mutate();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <section css={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <h2
        css={{
          ...theme.typography.h3,
          color: theme.colors.text.main,
          margin: 0,
        }}
      >
        documents
      </h2>
      <p
        css={{
          ...theme.typography.body2,
          color: theme.colors.text.muted,
          margin: 0,
          lineHeight: 1.5,
        }}
      >
        give the assistant your own notes to consult while answering. upload a
        text, markdown, or pdf file (manuals work great) and each chat turn
        quietly looks up the few most relevant passages and feeds them in as
        background. nothing leaves the box — files live in this user&apos;s
        sqlite store and are never shared with other accounts.
      </p>
      <Row
        label="embedding model"
        detail={
          <span
            css={{
              ...theme.typography.body2,
              color: theme.colors.text.muted,
            }}
          >
            the model that turns text into vectors. picks from whatever ollama
            has installed locally with the &quot;embedding&quot; capability.
            switch any time — already-uploaded documents keep their original
            vectors until you delete and re-upload them.
          </span>
        }
        theme={theme}
      >
        <select
          value={model}
          onChange={(e) => onModelChange(e.target.value)}
          disabled={models.length === 0}
          css={{
            ...theme.typography.body2,
            padding: "5px 8px",
            borderRadius: 4,
            border: `1px solid ${theme.colors.border}`,
            background: theme.colors.background.main,
            color: theme.colors.text.main,
            cursor: models.length === 0 ? "default" : "pointer",
            outline: "none",
          }}
        >
          {models.length === 0 && <option value="">none detected</option>}
          {models.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </Row>
      <Row
        label="upload"
        detail={
          <span
            css={{
              ...theme.typography.body2,
              color: theme.colors.text.muted,
            }}
          >
            plain text, markdown, or pdf. pdf text is extracted server-side (no
            ocr — image-only pdfs come up empty). text is split into overlapping
            ~800-character windows and embedded once on upload so chat stays
            fast.
          </span>
        }
        theme={theme}
      >
        <>
          <input
            ref={fileRef}
            type="file"
            accept=".txt,.md,.markdown,.pdf,text/plain,text/markdown,application/pdf"
            disabled={busy || !model}
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) void onPickFile(f);
            }}
            css={{ display: "none" }}
          />
          <button
            type="button"
            onClick={() => fileRef.current?.click()}
            disabled={busy || !model}
            css={{
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
            }}
          >
            {busy ? "ingesting…" : "pick file"}
          </button>
        </>
      </Row>
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
      <div css={{ display: "flex", flexDirection: "column", gap: 6 }}>
        {(docs ?? []).map((d) => (
          <div
            key={d.id}
            css={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "8px 10px",
              borderRadius: 4,
              border: `1px solid ${theme.colors.border}`,
              gap: 12,
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
              <span
                css={{
                  ...theme.typography.body2,
                  color: theme.colors.text.main,
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
                title={d.name}
              >
                {d.name}
              </span>
              <span
                css={{
                  ...theme.typography.caption,
                  color: theme.colors.text.muted,
                }}
              >
                {d.chunk_count} chunk{d.chunk_count === 1 ? "" : "s"} ·{" "}
                {Math.max(1, Math.round(d.size_bytes / 1024))} kb
              </span>
            </div>
            <button
              type="button"
              aria-label="delete document"
              onClick={() => void onDelete(d.id)}
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
                  color: theme.colors.error,
                },
              }}
            >
              <span className="material-icons-outlined" css={{ fontSize: 18 }}>
                close
              </span>
            </button>
          </div>
        ))}
        {docs && docs.length === 0 && (
          <div
            css={{
              ...theme.typography.caption,
              color: theme.colors.text.muted,
            }}
          >
            no documents yet
          </div>
        )}
      </div>
    </section>
  );
};

const AppearanceSection = ({ theme }: { theme: Theme }) => {
  const { override, setOverride } = useThemeOverride();
  return (
    <section css={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <h2
        css={{
          ...theme.typography.h3,
          color: theme.colors.text.main,
          margin: 0,
        }}
      >
        appearance
      </h2>
      <Row
        label="theme"
        detail={
          <span
            css={{
              ...theme.typography.body2,
              color: theme.colors.text.muted,
            }}
          >
            system follows the os preference; light / dark force the look
            regardless of prefers-color-scheme.
          </span>
        }
        theme={theme}
      >
        <select
          value={override}
          onChange={(e) => setOverride(e.target.value as ThemeOverride)}
          css={{
            ...theme.typography.body2,
            padding: "5px 8px",
            borderRadius: 4,
            border: `1px solid ${theme.colors.border}`,
            background: theme.colors.background.main,
            color: theme.colors.text.main,
            cursor: "pointer",
            outline: "none",
          }}
        >
          <option value="system">system</option>
          <option value="light">light</option>
          <option value="dark">dark</option>
        </select>
      </Row>
    </section>
  );
};

const VoiceSection = ({
  theme,
  ttsAvailable,
  sttAvailable,
}: {
  theme: Theme;
  ttsAvailable: boolean;
  sttAvailable: boolean;
}) => {
  const { data: voicesData } = useSWR(
    ttsAvailable ? "/api/voices" : null,
    api.voices,
  );
  const voices = useMemo(() => normalizeVoices(voicesData), [voicesData]);
  const [ttsOverride, setTtsOverride] = useState<string>(
    () => readVoiceOverride() ?? "auto",
  );
  const onTtsChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const v = e.target.value;
    setTtsOverride(v);
    writeVoiceOverride(v);
  };
  const [sttLang, setSttLang] = useState<SttLangPref>(() => readSttLangPref());
  const onSttChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const v = e.target.value as SttLangPref;
    setSttLang(v);
    writeSttLangPref(v);
  };
  const sttLabels: Record<(typeof SUPPORTED_STT_LANGS)[number], string> = {
    en: "english",
    fi: "suomi",
    sv: "svenska",
  };
  return (
    <section css={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <h2
        css={{
          ...theme.typography.h3,
          color: theme.colors.text.main,
          margin: 0,
        }}
      >
        voice
      </h2>
      {sttAvailable && (
        <Row
          label="dictation language"
          detail={
            <span
              css={{
                ...theme.typography.body2,
                color: theme.colors.text.muted,
              }}
            >
              whisper guesses language from the audio, but short chat utterances
              confuse it. auto follows your browser locale; pick a specific one
              if dictation keeps landing in the wrong tongue.
            </span>
          }
          theme={theme}
        >
          <select
            value={sttLang}
            onChange={onSttChange}
            css={{
              ...theme.typography.body2,
              padding: "5px 8px",
              borderRadius: 4,
              border: `1px solid ${theme.colors.border}`,
              background: theme.colors.background.main,
              color: theme.colors.text.main,
              cursor: "pointer",
              outline: "none",
            }}
          >
            <option value="auto">auto (browser locale)</option>
            {SUPPORTED_STT_LANGS.map((code) => (
              <option key={code} value={code}>
                {sttLabels[code]} · {code}
              </option>
            ))}
          </select>
        </Row>
      )}
      {ttsAvailable && (
        <Row
          label="read-aloud voice"
          detail={
            <span
              css={{
                ...theme.typography.body2,
                color: theme.colors.text.muted,
              }}
            >
              auto picks english / finnish based on the message; override here
              to force a specific voice.
            </span>
          }
          theme={theme}
        >
          <select
            value={ttsOverride}
            onChange={onTtsChange}
            css={{
              ...theme.typography.body2,
              padding: "5px 8px",
              borderRadius: 4,
              border: `1px solid ${theme.colors.border}`,
              background: theme.colors.background.main,
              color: theme.colors.text.main,
              cursor: "pointer",
              outline: "none",
            }}
          >
            <option value="auto">auto-detect</option>
            {voices.map((v) => (
              <option key={v.slug} value={v.slug}>
                {v.nameNative === v.nameEnglish
                  ? `${v.nameNative} · ${v.slug}`
                  : `${v.nameNative} (${v.nameEnglish}) · ${v.slug}`}
              </option>
            ))}
          </select>
        </Row>
      )}
    </section>
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
    <DeleteAccountRow me={me} theme={theme} />
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
