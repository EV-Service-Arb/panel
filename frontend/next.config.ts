import type { NextConfig } from "next";
import { PHASE_DEVELOPMENT_SERVER } from "next/constants";

/**
 * The panel is a static export: every screen reads `/api/v1` from the browser
 * with the session cookie, so there is nothing for a Node server to render. The
 * Rust binary serves `out/` next to `/api` and `/auth` on one origin (see
 * README.md, "How it is served").
 *
 * `next dev` instead proxies `/api`, `/auth` and the sites' bundle to a backend — the real one,
 * or `npm run dev:stub` — so the cookies and the CSRF header behave as in production.
 */
export default function config(phase: string): NextConfig {
  if (phase === PHASE_DEVELOPMENT_SERVER) {
    const backend = process.env.PANEL_DEV_BACKEND;
    if (!backend) throw new Error("PANEL_DEV_BACKEND: the origin `next dev` proxies to, `panel serve`'s or `npm run dev:stub`'s");
    return {
      // Next writes AGENTS.md and CLAUDE.md into the project on `next dev` otherwise.
      agentRules: false,
      // A trailing-slash redirect would run before the rewrite and bounce /api/v1/me.
      skipTrailingSlashRedirect: true,
      async rewrites() {
        return [
          { source: "/api/:path*", destination: `${backend}/api/:path*` },
          { source: "/auth/:path*", destination: `${backend}/auth/:path*` },
          { source: "/sites/mfe/:path*", destination: `${backend}/sites/mfe/:path*` },
          { source: "/review_archive/mfe/:path*", destination: `${backend}/review_archive/mfe/:path*` },
          { source: "/grafana/:path*", destination: `${backend}/grafana/:path*` },
          // an element's views are its page, as `panel serve` answers them (web.rs, ELEMENT_PAGES)
          { source: "/review_archive/:view+", destination: "/review_archive" },
          { source: "/sites/:view+", destination: "/sites" },
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
