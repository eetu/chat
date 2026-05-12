import { createContext, use } from "react";

export type ThemeOverride = "system" | "light" | "dark";
export const THEME_OVERRIDE_KEY = "chat:theme";

export const readThemeOverride = (): ThemeOverride => {
  try {
    const v = window.localStorage.getItem(THEME_OVERRIDE_KEY);
    if (v === "light" || v === "dark") return v;
  } catch {
    // ignore (private mode / quota)
  }
  return "system";
};

export const writeThemeOverride = (value: ThemeOverride) => {
  try {
    window.localStorage.setItem(THEME_OVERRIDE_KEY, value);
  } catch {
    // ignore
  }
};

/// Provided by `<Root>`. Settings page consumes this to flip the
/// override at runtime without a reload.
export const ThemeOverrideContext = createContext<{
  override: ThemeOverride;
  setOverride: (next: ThemeOverride) => void;
}>({
  override: "system",
  setOverride: () => {
    // default noop — only meaningful when Root has mounted the provider
  },
});

export const useThemeOverride = () => use(ThemeOverrideContext);
