import "katex/dist/katex.min.css";

import { Global, useTheme } from "@emotion/react";
import { ComponentProps, memo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";

const isExternal = (href: string | undefined) =>
  !!href && /^https?:\/\//i.test(href);

/**
 * Pre-process streaming markdown so a half-arrived fenced code block still
 * renders as code. While a model is mid-stream a ``` may have opened
 * without its closing partner — without rebalancing, rehype-highlight
 * either drops the block or treats the rest of the message as code.
 */
const balanceFences = (s: string) => {
  const matches = s.match(/```/g);
  if (matches && matches.length % 2 === 1) {
    return `${s}\n\`\`\`\n`;
  }
  return s;
};

const CodeBlock = ({ children, ...rest }: ComponentProps<"pre">) => {
  const theme = useTheme();
  const preRef = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    const text = preRef.current?.textContent ?? "";
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard write can fail on non-secure contexts; swallow silently
    }
  };

  return (
    <div css={{ position: "relative" }}>
      <pre ref={preRef} {...rest}>
        {children}
      </pre>
      <button
        type="button"
        onClick={handleCopy}
        aria-label={copied ? "code copied" : "copy code"}
        css={{
          position: "absolute",
          top: 6,
          right: 6,
          padding: "2px 8px",
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
          fontSize: 11,
          color: theme.colors.text.muted,
          background: theme.colors.background.main,
          border: `1px solid ${theme.colors.border}`,
          borderRadius: 4,
          cursor: "pointer",
          opacity: 0.75,
          "&:hover, &:focus-visible": {
            opacity: 1,
            color: theme.colors.text.main,
          },
        }}
      >
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
};

const components: ComponentProps<typeof ReactMarkdown>["components"] = {
  pre: ({ children, ...rest }) => <CodeBlock {...rest}>{children}</CodeBlock>,
  a: ({ href, children, ...rest }) =>
    isExternal(href) ? (
      <a {...rest} href={href} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    ) : (
      <a {...rest} href={href}>
        {children}
      </a>
    ),
  table: ({ children, ...rest }) => (
    <div
      css={{
        overflowX: "auto",
        margin: "0.5em 0",
        WebkitOverflowScrolling: "touch",
      }}
    >
      <table {...rest}>{children}</table>
    </div>
  ),
  img: ({ alt, ...rest }) => (
    <img
      {...rest}
      alt={alt ?? ""}
      loading="lazy"
      css={{
        maxWidth: "100%",
        height: "auto",
        borderRadius: 4,
        display: "block",
      }}
    />
  ),
};

/**
 * Render assistant content as markdown. Streaming-safe: re-parses on every
 * delta but content is small enough that this is cheap.
 *
 * Code highlighting via rehype-highlight (highlight.js). The two highlight
 * themes are loaded once globally and toggled by `prefers-color-scheme`.
 *
 * SECURITY: rehype-raw is intentionally NOT used. Raw HTML in the model's
 * output is escaped, not rendered.
 */
const Markdown = ({ children }: { children: string }) => {
  const theme = useTheme();
  const codeBg =
    theme.mode === "dark" ? "rgba(255,255,255,0.04)" : "rgba(0,0,0,0.04)";
  const fenceBg = theme.mode === "dark" ? "#1a1a1a" : "#f6f7f9";

  return (
    <>
      <Global styles={highlightThemes} />
      <div
        css={{
          ...theme.typography.body1,
          color: theme.colors.text.main,
          lineHeight: 1.55,
          "p, ul, ol, pre, blockquote, table": { margin: "0.5em 0" },
          "p:first-of-type": { marginTop: 0 },
          "p:last-of-type": { marginBottom: 0 },
          "ul, ol": { paddingLeft: "1.4em" },
          "li + li": { marginTop: 4 },
          a: {
            color: theme.colors.activity.on,
            textDecoration: "underline",
            textUnderlineOffset: 2,
          },
          "h1, h2, h3, h4": {
            fontFamily: theme.fonts.heading,
            fontWeight: 500,
            margin: "1em 0 0.4em",
            lineHeight: 1.25,
          },
          h1: { fontSize: 22 },
          h2: { fontSize: 19 },
          h3: { fontSize: 17 },
          h4: { fontSize: 15 },
          "code:not(pre code)": {
            background: codeBg,
            padding: "1px 5px",
            borderRadius: 4,
            fontSize: "0.9em",
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
          },
          pre: {
            background: fenceBg,
            border: `1px solid ${theme.colors.border}`,
            borderRadius: theme.border.radius,
            padding: "10px 12px",
            overflowX: "auto",
            fontSize: "0.88em",
            lineHeight: 1.5,
          },
          "pre code": {
            background: "transparent",
            padding: 0,
            fontFamily:
              "ui-monospace, SFMono-Regular, Menlo, Monaco, monospace",
          },
          blockquote: {
            borderLeft: `3px solid ${theme.colors.border}`,
            paddingLeft: 12,
            color: theme.colors.text.muted,
            margin: "0.5em 0",
          },
          table: {
            borderCollapse: "collapse",
            width: "100%",
            fontSize: "0.92em",
          },
          "th, td": {
            border: `1px solid ${theme.colors.border}`,
            padding: "4px 8px",
            textAlign: "left",
          },
          th: {
            background: theme.colors.background.light,
            fontFamily: theme.fonts.heading,
            fontWeight: 500,
          },
          hr: {
            border: "none",
            borderTop: `1px solid ${theme.colors.border}`,
            margin: "1em 0",
          },
          ".katex-display": {
            margin: "0.6em 0",
            overflowX: "auto",
            overflowY: "hidden",
          },
          ".katex": {
            fontSize: "1.05em",
          },
        }}
      >
        <ReactMarkdown
          remarkPlugins={[remarkGfm, remarkBreaks, remarkMath]}
          rehypePlugins={[
            [rehypeHighlight, { detect: true, ignoreMissing: true }],
            [rehypeKatex, { strict: false, output: "html" }],
          ]}
          components={components}
        >
          {balanceFences(children)}
        </ReactMarkdown>
      </div>
    </>
  );
};

/**
 * highlight.js github / github-dark, scoped to .hljs and toggled via
 * prefers-color-scheme. Keeping inline so we don't ship a separate CSS
 * import path.
 */
const highlightThemes = `
.hljs { color: #24292f; background: transparent; }
.hljs-doctag,.hljs-keyword,.hljs-meta .hljs-keyword,.hljs-template-tag,.hljs-template-variable,.hljs-type,.hljs-variable.language_ { color: #cf222e; }
.hljs-title,.hljs-title.class_,.hljs-title.class_.inherited__,.hljs-title.function_ { color: #8250df; }
.hljs-attr,.hljs-attribute,.hljs-literal,.hljs-meta,.hljs-number,.hljs-operator,.hljs-selector-attr,.hljs-selector-class,.hljs-selector-id,.hljs-variable { color: #0550ae; }
.hljs-meta .hljs-string,.hljs-regexp,.hljs-string { color: #0a3069; }
.hljs-built_in,.hljs-symbol { color: #e36209; }
.hljs-code,.hljs-comment,.hljs-formula { color: #6e7781; font-style: italic; }
.hljs-name,.hljs-quote,.hljs-selector-pseudo,.hljs-selector-tag { color: #116329; }
.hljs-subst { color: #24292f; }
.hljs-section { color: #0550ae; font-weight: 600; }
.hljs-bullet { color: #735c0f; }
.hljs-emphasis { font-style: italic; }
.hljs-strong { font-weight: 600; }
.hljs-addition { color: #116329; background: #dafbe1; }
.hljs-deletion { color: #82071e; background: #ffebe9; }

@media (prefers-color-scheme: dark) {
  .hljs { color: #c9d1d9; }
  .hljs-doctag,.hljs-keyword,.hljs-meta .hljs-keyword,.hljs-template-tag,.hljs-template-variable,.hljs-type,.hljs-variable.language_ { color: #ff7b72; }
  .hljs-title,.hljs-title.class_,.hljs-title.class_.inherited__,.hljs-title.function_ { color: #d2a8ff; }
  .hljs-attr,.hljs-attribute,.hljs-literal,.hljs-meta,.hljs-number,.hljs-operator,.hljs-selector-attr,.hljs-selector-class,.hljs-selector-id,.hljs-variable { color: #79c0ff; }
  .hljs-meta .hljs-string,.hljs-regexp,.hljs-string { color: #a5d6ff; }
  .hljs-built_in,.hljs-symbol { color: #ffa657; }
  .hljs-code,.hljs-comment,.hljs-formula { color: #8b949e; }
  .hljs-name,.hljs-quote,.hljs-selector-pseudo,.hljs-selector-tag { color: #7ee787; }
  .hljs-subst { color: #c9d1d9; }
  .hljs-section { color: #1f6feb; }
  .hljs-bullet { color: #f2cc60; }
  .hljs-addition { color: #aff5b4; background: #033a16; }
  .hljs-deletion { color: #ffdcd7; background: #67060c; }
}
`;

export default memo(Markdown);
