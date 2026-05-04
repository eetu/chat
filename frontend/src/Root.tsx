import { Global, ThemeProvider } from "@emotion/react";
import { ReactNode } from "react";
import { useMediaQuery } from "usehooks-ts";

import { darkTheme, lightTheme } from "./themes";

const Root = ({ children }: { children: ReactNode }) => {
  const isDarkTheme = useMediaQuery("(prefers-color-scheme: dark)");
  const theme = isDarkTheme ? darkTheme : lightTheme;

  return (
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
  );
};

export default Root;
