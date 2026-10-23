import type { NextConfig } from "next";
import { PHASE_DEVELOPMENT_SERVER } from "next/constants";

/**
 * The panel is a static export: every screen reads `/api/v1` from the browser
 * with the session cookie, so there is nothing for a Node server to render. The
 * Rust binary serves `out/` next to `/api` and `/auth` on one origin (see
 * README.md, "How it is served").
 *
 * `next dev` instead proxies `/api` and `/auth` to a backend — the real one, or
 * `npm run dev:stub` — so the cookies and the CSRF header behave as in production.
 */
export default function config(phase: string): NextConfig {
  if (phase === PHASE_DEVELOPMENT_SERVER) {
    const backend = process.env.PANEL_DEV_BACKEND ?? "http://127.0.0.1:3121";
    return {
      // Next writes AGENTS.md and CLAUDE.md into the project on `next dev` otherwise.
      agentRules: false,
      // A trailing-slash redirect would run before the rewrite and bounce /api/v1/me.
      skipTrailingSlashRedirect: true,
      async rewrites() {
        return [
          { source: "/api/:path*", destination: `${backend}/api/:path*` },
          { source: "/auth/:path*", destination: `${backend}/auth/:path*` },
        ];
      },
    };
  }
  return {
    output: "export",
    // `leads/index.html` rather than `leads.html`: a directory per screen is what
    // a static file server resolves without per-route rules.
    trailingSlash: true,
    images: { unoptimized: true },
  };
}
