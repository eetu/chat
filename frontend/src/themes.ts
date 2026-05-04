import { Theme } from "@emotion/react";

type Typography = {
  fontSize: string;
  fontWeight: number | string;
  fontFamily?: string;
};

declare module "@emotion/react" {
  export interface Theme {
    mode: string;
    border: {
      radius: number | string;
    };
    fonts: {
      body: string;
      heading: string;
      display: string;
    };
    colors: {
      body: string;
      text: {
        main: string;
        muted: string;
        light: string;
      };
      background: {
        main: string;
        light: string;
      };
      error: string;
      border: string;
      activity: {
        on: string;
        onBackground: string;
        onSoft: string;
        offBackground: string;
      };
      connected: string;
      disconnected: string;
      warm: string;
      cool: string;
      rain: string;
      pv: string;
      battery: string;
      home: string;
      grid: string;
      soc: string;
    };
    typography: {
      h1: Typography;
      h2: Typography;
      h3: Typography;
      body1: Typography;
      body2: Typography;
      caption: Typography;
      label: Typography;
    };
    shadows: {
      main: string;
    };
  }
}

const fonts = {
  body: '"Inter", system-ui, sans-serif',
  heading: '"Space Grotesk", "Inter", sans-serif',
  display: '"Abril Fatface", "Space Grotesk", monospace',
};

const typography = {
  h1: { fontSize: "50px", fontWeight: 400, fontFamily: fonts.body },
  h2: { fontSize: "20px", fontWeight: 500, fontFamily: fonts.heading },
  h3: { fontSize: "18px", fontWeight: 500, fontFamily: fonts.heading },
  body1: { fontSize: "16px", fontWeight: 400, fontFamily: fonts.body },
  body2: { fontSize: "14px", fontWeight: 400, fontFamily: fonts.body },
  caption: { fontSize: "13px", fontWeight: 300, fontFamily: fonts.body },
  label: { fontSize: "14px", fontWeight: 500, fontFamily: fonts.heading },
};

export const lightTheme: Theme = {
  mode: "light",
  border: { radius: 6 },
  fonts,
  colors: {
    body: "#f0f0f0",
    text: {
      main: "#525252",
      muted: "#a0a0a0",
      light: "#e9e9e9",
    },
    background: {
      main: "#fff",
      light: "#fbfbfb",
    },
    error: "tomato",
    border: "lightgray",
    activity: {
      on: "#f78f08",
      onBackground:
        "linear-gradient(153deg, rgba(255,237,207,1) 0%, rgba(255,239,171,1) 56%)",
      onSoft: "rgba(247, 143, 8, 0.10)",
      offBackground: "#d9d9d9",
    },
    connected: "#4caf50",
    disconnected: "#f44336",
    warm: "#e65100",
    cool: "#1565c0",
    rain: "#94daf7",
    pv: "#f5a524",
    battery: "#5fb3a3",
    home: "#5b8fc2",
    grid: "#f5a524",
    soc: "#d65a8a",
  },
  typography,
  shadows: {
    main: "rgba(60, 64, 67, 0.3) 0px 1px 2px 0px, rgba(60, 64, 67, 0.15) 0px 2px 6px 2px",
  },
};

export const darkTheme: Theme = {
  mode: "dark",
  border: { radius: 6 },
  fonts,
  colors: {
    body: "#0f0f0f",
    text: {
      main: "#d6d6d6",
      muted: "#8a8a8a",
      light: "#646464",
    },
    background: {
      main: "#252525",
      light: "#1c1c1c",
    },
    error: "pink",
    border: "#1f1f1f",
    activity: {
      on: "#f78f08",
      onBackground: "rgba(247, 143, 8, 0.2)",
      onSoft: "rgba(247, 143, 8, 0.20)",
      offBackground: "#404040",
    },
    connected: "#4caf50",
    disconnected: "#f44336",
    warm: "#ff7043",
    cool: "#42a5f5",
    rain: "#43529c",
    pv: "#a35a00",
    battery: "#5fb3a3",
    home: "#7aa3d1",
    grid: "#f5a524",
    soc: "#d65a8a",
  },
  typography,
  shadows: {
    main: "none",
  },
};
