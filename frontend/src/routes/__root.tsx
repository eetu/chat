/* eslint-disable react-refresh/only-export-components */
import { useTheme } from "@emotion/react";
import { createRootRoute, Outlet, useLocation } from "@tanstack/react-router";
import { useState } from "react";
import { useMediaQuery } from "usehooks-ts";

import LoginGate from "../components/LoginGate";
import Sidebar from "../components/Sidebar";

const RootLayout = () => {
  const theme = useTheme();
  const isMobile = useMediaQuery("(max-width: 600px)");

  // single visibility flag. desktop default = open inline; mobile default = closed.
  const [open, setOpen] = useState(!isMobile);
  const location = useLocation();
  const [lastMobile, setLastMobile] = useState(isMobile);
  const [lastPath, setLastPath] = useState(location.pathname);

  // crossing the breakpoint resets to layout default; route change closes
  // the drawer on mobile. Done during render so React can batch with the
  // commit and skip the wasted second render of an effect-based version.
  if (lastMobile !== isMobile) {
    setLastMobile(isMobile);
    setLastPath(location.pathname);
    setOpen(!isMobile);
  } else if (lastPath !== location.pathname) {
    setLastPath(location.pathname);
    if (isMobile) setOpen(false);
  }

  return (
    <LoginGate>
      <div
        css={{
          display: "flex",
          width: "100%",
          height: "100%",
          background: theme.colors.body,
          position: "relative",
        }}
      >
        {/* desktop: inline, hidden when collapsed.
            mobile: fixed off-canvas drawer, transforms in/out. */}
        <div
          css={{
            position: isMobile ? "fixed" : "static",
            zIndex: 20,
            top: 0,
            bottom: 0,
            left: 0,
            transform:
              isMobile && !open ? "translateX(-100%)" : "translateX(0)",
            transition: "transform 200ms ease",
            boxShadow: isMobile && open ? "rgba(0,0,0,0.25) 0 0 24px" : "none",
            display: !isMobile && !open ? "none" : "flex",
          }}
        >
          <Sidebar onClose={() => setOpen(false)} />
        </div>

        {isMobile && open && (
          <div
            onClick={() => setOpen(false)}
            css={{
              position: "fixed",
              inset: 0,
              background: "rgba(0,0,0,0.4)",
              zIndex: 15,
            }}
          />
        )}

        <main
          css={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
            minWidth: 0,
            width: "100%",
            position: "relative",
          }}
        >
          {!open && (
            <button
              type="button"
              aria-label="open menu"
              onClick={() => setOpen(true)}
              css={{
                position: "absolute",
                top: 12,
                left: 12,
                zIndex: 10,
                width: 36,
                height: 36,
                borderRadius: "50%",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                border: `1px solid ${theme.colors.border}`,
                background: theme.colors.background.main,
                color: theme.colors.text.main,
                cursor: "pointer",
                boxShadow: theme.shadows.main,
                "&:hover": {
                  background: theme.colors.background.light,
                },
              }}
            >
              <span className="material-icons-outlined" css={{ fontSize: 20 }}>
                menu
              </span>
            </button>
          )}
          <Outlet />
        </main>
      </div>
    </LoginGate>
  );
};

export const Route = createRootRoute({
  component: RootLayout,
});
