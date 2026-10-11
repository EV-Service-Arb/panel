import { ROUTES } from "@/shared/config/routes";

// The panel's own screens: a chip link to one of them is a soft navigation through the router.
// Deliberately absent: the review archive's own views (tokens, act-as — the chip's `external`
// items) and Grafana, which the backend forwards to other services, and everything outside the
// app (`/auth`, `/api`, `/cabinet`): those stay full navigations.
const SOFT_ROUTES: readonly string[] = [
  "/",
  ROUTES.overview,
  ROUTES.leads,
  ROUTES.places,
  ROUTES.pricing,
  ROUTES.experiments,
  ROUTES.sources,
  ROUTES.more,
  ROUTES.account,
  ROUTES.reviewArchive,
];

// The export is built with `trailingSlash`, so `/account` and `/account/` are one screen.
const bare = (path: string) => (path.length > 1 ? path.replace(/\/+$/, "") : path);

const BY_PATH = new Map(SOFT_ROUTES.map((route) => [bare(route), route]));

// Only resolves relative paths; never fetched.
const BASE = "http://panel.invalid";

/**
 * The router target for a chip link (`ev:navigate`'s `detail.href`, path + search + hash), or
 * null when it is not one of the panel's screens and the browser should follow the link itself.
 * The target is spelled as the panel's own `<Link href>`s spell it (`ROUTES`), query and hash kept.
 */
export function panelRouteFor(href: unknown): string | null {
  if (typeof href !== "string" || !href.startsWith("/") || href.startsWith("//") || href.startsWith("/\\")) return null;
  let url: URL;
  try {
    url = new URL(href, BASE);
  } catch {
    return null;
  }
  if (url.origin !== BASE) return null;
  const route = BY_PATH.get(bare(url.pathname));
  return route === undefined ? null : route + url.search + url.hash;
}
