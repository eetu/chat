import { playwright } from "@vitest/browser-playwright";
import { configDefaults, defineConfig, mergeConfig } from "vitest/config";

import viteConfig from "./vite.config";

// Same two-project layout as sibling halo: a node-environment lane for
// pure logic specs and a chromium lane (via @vitest/browser-playwright)
// for React component tests. Browser specs are the only ones that pull
// in JSX/DOM, so the unit lane stays cheap and synchronous.
export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      projects: [
        {
          test: {
            name: "unit",
            globals: true,
            environment: "node",
            include: ["src/**/*.{test,spec}.?(c|m)[jt]s"],
            exclude: [
              ...configDefaults.exclude,
              "**/*.browser.{test,spec}.*",
              "**/e2e-tests/**",
              "**/playwright.configuration.ts",
            ],
          },
        },
        {
          test: {
            name: "browser",
            globals: true,
            include: ["src/**/*.browser.{test,spec}.?(c|m)[jt]s?(x)"],
            exclude: [
              ...configDefaults.exclude,
              "**/e2e-tests/**",
              "**/playwright.configuration.ts",
            ],
            browser: {
              enabled: true,
              headless: true,
              provider: playwright(),
              instances: [{ browser: "chromium" }],
            },
          },
        },
      ],
    },
  }),
);
