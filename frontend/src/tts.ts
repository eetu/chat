/// Helpers for the piper-tts integration. Keeping detection + voice
/// picking in one module so the settings page and the per-message
/// read-aloud action share the same logic.

export const TTS_VOICE_KEY = "chat:ttsVoice";

export type VoiceInfo = {
  slug: string;
  /** ISO-639-1 family — `en`, `fi`. Used by `pickVoice` to match a
   * loaded voice against the detected text language. */
  family: string;
  /** BCP-47-ish code — `en_US`, `fi_FI`. */
  code: string;
  nameEnglish: string;
  nameNative: string;
};

/// Flatten piper's `/voices` payload into VoiceInfo records. Piper's
/// current shape is a top-level object keyed by slug whose values carry
/// rich `language` metadata; older builds (and a few forks) wrapped a
/// flat slug list in `{ voices: [...] }`. Accept both so a future
/// upstream tweak doesn't silently break the picker.
export const normalizeVoices = (data: unknown): VoiceInfo[] => {
  if (!data || typeof data !== "object") return [];

  // Legacy / fork shape: { voices: ["slug", ...] | [{ name }] }
  const arr = (data as { voices?: unknown }).voices;
  if (Array.isArray(arr)) {
    return arr
      .map((v): VoiceInfo | null => {
        const slug =
          typeof v === "string"
            ? v
            : v && typeof v === "object"
              ? ((v as { name?: string; voice?: string }).name ??
                (v as { voice?: string }).voice ??
                "")
              : "";
        if (!slug) return null;
        return slugFallback(slug);
      })
      .filter((v): v is VoiceInfo => v !== null);
  }

  // Current shape: { "<slug>": { language: { family, code, ... }, ... } }
  return Object.entries(data as Record<string, unknown>).map(([slug, info]) => {
    const language =
      info && typeof info === "object"
        ? ((info as { language?: unknown }).language as
            | Record<string, unknown>
            | undefined)
        : undefined;
    const family = typeof language?.family === "string" ? language.family : "";
    const code = typeof language?.code === "string" ? language.code : "";
    const nameEnglish =
      typeof language?.name_english === "string"
        ? (language.name_english as string)
        : slug;
    const nameNative =
      typeof language?.name_native === "string"
        ? (language.name_native as string)
        : nameEnglish;
    return {
      slug,
      family: family || slug.split("_")[0] || "",
      code: code || slug.split("-")[0] || "",
      nameEnglish,
      nameNative,
    };
  });
};

/// Derive a best-effort `VoiceInfo` from a bare slug string when the
/// upstream returned no metadata. Splits on the conventional
/// `<lang>_<COUNTRY>-<dataset>-<quality>` format.
const slugFallback = (slug: string): VoiceInfo => {
  const code = slug.split("-")[0] ?? slug;
  const family = code.split("_")[0] ?? code;
  return { slug, family, code, nameEnglish: slug, nameNative: slug };
};

/// Best-effort language detection. Returns `null` when the signal is
/// too weak to call so callers can fall back to a different text
/// source (e.g. assistant body when the user-turn detection is null).
export const detectLang = (text: string): "en" | "fi" | null => {
  if (!text) return null;
  const lower = text.toLowerCase();
  // Common Finnish function words + double-vowel bigrams that rarely
  // appear in en / sv / de prose.
  const fiHints =
    /\b(että|olen|on|kun|niin|tämä|ovat|hän|mutta|ja|sinä|minä|joka|kuin|mitä|kiitos|hei)\b|ää|öö/;
  if (fiHints.test(lower)) return "fi";
  if (
    /\b(the|and|with|that|this|have|been|from|your|will|about|which)\b/.test(
      lower,
    )
  ) {
    return "en";
  }
  return null;
};

/// Pick a loaded piper voice for this turn. Prefers the per-user
/// override stored in localStorage; falls back to language detection on
/// the user's prior turn (most reliable language signal), then on the
/// assistant text, then on the first voice loaded upstream.
export const pickVoice = (
  assistantText: string,
  priorUserText: string | undefined,
  voices: VoiceInfo[],
  override: string | null,
): string | undefined => {
  if (voices.length === 0) return undefined;
  if (
    override &&
    override !== "auto" &&
    voices.some((v) => v.slug === override)
  ) {
    return override;
  }
  const lang =
    detectLang(priorUserText ?? "") ?? detectLang(assistantText) ?? "en";
  return voices.find((v) => v.family === lang)?.slug ?? voices[0]?.slug;
};

export const readVoiceOverride = (): string | null => {
  try {
    return window.localStorage.getItem(TTS_VOICE_KEY);
  } catch {
    return null;
  }
};

export const writeVoiceOverride = (value: string) => {
  try {
    window.localStorage.setItem(TTS_VOICE_KEY, value);
  } catch {
    // ignore (private mode / quota)
  }
};
