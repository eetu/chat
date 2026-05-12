import { Global, ThemeProvider } from "@emotion/react";
import { ReactNode, useCallback, useState } from "react";
import { useMediaQuery } from "usehooks-ts";

import {
  readThemeOverride,
  ThemeOverride,
  ThemeOverrideContext,
  writeThemeOverride,
} from "./theme";
import { darkTheme, lightTheme } from "./themes";

const Root = ({ children }: { children: ReactNode }) => {
  const isDarkSystem = useMediaQuery("(prefers-color-scheme: dark)");
  const [override, setOverride] = useState<ThemeOverride>(() =>
    readThemeOverride(),
  );

  const setOverrideAndPersist = useCallback((next: ThemeOverride) => {
    writeThemeOverride(next);
    setOverride(next);
  }, []);

  const useDark =
    override === "dark" || (override === "system" && isDarkSystem);
  const theme = useDark ? darkTheme : lightTheme;

  return (
    <ThemeOverrideContext
      value={{ override, setOverride: setOverrideAndPersist }}
    >
      <ThemeProvider theme={theme}>
        <Global
          styles={{
            html: {
              fontFamily: theme.fonts.body,
              height: "100%",
            },
            body: {
              padding: 0,
              margin: 0,
              height: "100%",
              backgroundColor: theme.colors.body,
              color: theme.colors.text.main,
              WebkitFontSmoothing: "antialiased",
            },
            a: { color: "inherit", textDecoration: "none" },
            "*": { boxSizing: "border-box" },
            "#root": {
              height: "100%",
              display: "flex",
            },
            "input, textarea, button": {
              fontFamily: "inherit",
              color: "inherit",
            },
          }}
        />
        {children}
      </ThemeProvider>
    </ThemeOverrideContext>
  );
};

export default Root;
