/** The double-submit token the backend sets beside the session (`sa_server::cookies`). */
const CSRF_COOKIE = "sa_csrf";
export const CSRF_HEADER = "x-sa-csrf";

/**
 * The CSRF token from a `document.cookie` string. Behind https the backend names
 * it `__Host-sa_csrf`, over plain http (development) `sa_csrf`; the prefixed one
 * wins when both are there, as it does on the server.
 */
export function readCsrf(cookie: string): string | null {
  const pairs = cookie
    .split(";")
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => {
      const at = p.indexOf("=");
      return at < 0 ? ([p, ""] as const) : ([p.slice(0, at), p.slice(at + 1)] as const);
    });
  const find = (name: string) => pairs.find(([k, v]) => k === name && v !== "")?.[1] ?? null;
  return find(`__Host-${CSRF_COOKIE}`) ?? find(CSRF_COOKIE);
}

/** The name the backend gives the cookie on this page's scheme, for an element that reads it itself; browser only. */
export function csrfCookieName(): string {
  return window.location.protocol === "https:" ? `__Host-${CSRF_COOKIE}` : CSRF_COOKIE;
}
