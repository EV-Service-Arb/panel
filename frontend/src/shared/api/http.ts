import { type Parser, parse } from "@/shared/lib/parse";

import { CSRF_HEADER, readCsrf } from "./csrf";
import { requestSignInOverlay } from "./signin-overlay";

/**
 * Why a call failed, in the words the screens act on. The backend's gate
 * (`panel_server::signin::gate`) answers 401 for no session, 403 with
 * `"no access to the panel"` for a signed-in person without a grant, 403 `"csrf"`
 * for a write without the header, and 503 while concierge is unreachable —
 * which keeps the session, so the front end must not sign anyone out on it.
 */
export type ApiFailure =
  | { kind: "unauthenticated" }
  | { kind: "no_access" }
  | { kind: "csrf" }
  | { kind: "forbidden"; message: string }
  | { kind: "unavailable" }
  | { kind: "not_found" }
  /** `body` is the whole answer: some 409s carry the record as it now is (`current`). */
  | { kind: "conflict"; message: string; body: unknown }
  | { kind: "bad_request"; message: string }
  /** A 422 naming the fields it refused, each with the backend's reason. */
  | { kind: "invalid_fields"; fields: Record<string, string> }
  /** A 422 naming one place in the body it refused (`needs.standard.inputs[2]`). */
  | { kind: "invalid_path"; path: string; message: string }
  | { kind: "failed"; status: number; message: string }
  | { kind: "network" };

export class ApiError extends Error {
  override name = "ApiError";
  constructor(readonly failure: ApiFailure) {
    super(`api: ${failure.kind}`);
  }
}

export interface HttpDeps {
  fetch: typeof fetch;
  /** The page's cookies, as `document.cookie` has them. */
  cookie: () => string;
  /** A 401: the session is gone. The browser goes to the backend's sign-in. */
  onUnauthenticated: () => void;
}

export type Query = Record<string, string | number | boolean | null | undefined>;

function withQuery(path: string, query?: Query): string {
  if (!query) return path;
  const qs = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) if (v !== undefined && v !== null && v !== "") qs.set(k, String(v));
  const s = qs.toString();
  return s ? `${path}?${s}` : path;
}

interface ErrorBody {
  message: string;
  fields: Record<string, string> | null;
  path: string | null;
  body: unknown;
}

const NO_BODY: ErrorBody = { message: "", fields: null, path: null, body: null };

/** `{"error": "...", "fields": {...}}` or `{"error": "...", "path": "..."}`, the backend's error shapes; anything else says nothing. */
async function errorBody(res: Response): Promise<ErrorBody> {
  try {
    const body: unknown = await res.json();
    if (typeof body !== "object" || body === null) return NO_BODY;
    const message = "error" in body && typeof body.error === "string" ? body.error : "";
    const raw = "fields" in body ? body.fields : null;
    const fields =
      typeof raw === "object" && raw !== null && !Array.isArray(raw)
        ? Object.fromEntries(Object.entries(raw).filter((e): e is [string, string] => typeof e[1] === "string"))
        : null;
    const path = "path" in body && typeof body.path === "string" ? body.path : null;
    return { message, fields, path, body };
  } catch {
    // Not JSON: a proxy's page, say. The status says enough.
    return NO_BODY;
  }
}

export async function failureOf(res: Response): Promise<ApiFailure> {
  const { message, fields, path, body } = await errorBody(res);
  switch (res.status) {
    case 401:
      return { kind: "unauthenticated" };
    case 403:
      if (message === "csrf") return { kind: "csrf" };
      if (message === "no access to the panel") return { kind: "no_access" };
      return { kind: "forbidden", message };
    case 404:
      return { kind: "not_found" };
    case 409:
      return { kind: "conflict", message, body };
    case 400:
      return { kind: "bad_request", message };
    case 422:
      if (fields) return { kind: "invalid_fields", fields };
      return path === null ? { kind: "failed", status: 422, message } : { kind: "invalid_path", path, message };
    case 503:
      return { kind: "unavailable" };
    default:
      return { kind: "failed", status: res.status, message };
  }
}

/** What a write may ask of `fetch` beyond the defaults. `keepalive`: the request outlives the page, for a write that a navigation follows at once. */
export type SendInit = Pick<RequestInit, "keepalive">;

export interface Http {
  get<T>(path: string, parser: Parser<T>, query?: Query): Promise<T>;
  /** Every write carries the CSRF header; the backend refuses one without it. `headers` adds others (an Idempotency-Key). */
  send<T>(
    method: "POST" | "PUT" | "DELETE",
    path: string,
    body: unknown,
    parser: Parser<T>,
    headers?: Readonly<Record<string, string>>,
    init?: SendInit,
  ): Promise<T>;
}

export function createHttp(deps: HttpDeps): Http {
  async function call<T>(path: string, init: RequestInit, parser: Parser<T>): Promise<T> {
    let res: Response;
    try {
      res = await deps.fetch(path, { credentials: "same-origin", cache: "no-store", ...init });
    } catch {
      throw new ApiError({ kind: "network" });
    }
    if (!res.ok) {
      const failure = await failureOf(res);
      if (failure.kind === "unauthenticated") deps.onUnauthenticated();
      throw new ApiError(failure);
    }
    const body: unknown = res.status === 204 ? null : await res.json();
    return parse(parser, body);
  }

  return {
    get: (path, parser, query) => call(withQuery(path, query), { method: "GET", headers: { accept: "application/json" } }, parser),
    send: (method, path, body, parser, extra, options) => {
      const headers: Record<string, string> = { ...extra, accept: "application/json" };
      const csrf = readCsrf(deps.cookie());
      if (csrf) headers[CSRF_HEADER] = csrf;
      const init: RequestInit = { ...options, method, headers };
      if (body !== undefined) {
        headers["content-type"] = "application/json";
        init.body = JSON.stringify(body);
      }
      return call(path, init, parser);
    },
  };
}

/** The backend's sign-in; a full navigation, since it answers with a redirect to concierge. */
export const SIGN_IN_PATH = "/auth/login";

/** The sign-in through Google's account chooser, past the evinvest.ltd session the browser may have. */
export function switchAccountPath(returnTo: string): string {
  return `${SIGN_IN_PATH}?prompt=select_account&return_to=${encodeURIComponent(returnTo)}`;
}

/** The full-page way to sign in: off to the backend's sign-in, and back here. */
export function redirectToSignIn(): void {
  if (typeof window === "undefined") return;
  const here = window.location.pathname + window.location.search;
  // /auth/login is the Rust backend's route, not a page of this app: only a full navigation reaches it.
  // eslint-disable-next-line @next/next/no-location-assign-relative-destination
  window.location.assign(`${SIGN_IN_PATH}?return_to=${encodeURIComponent(here)}`);
}

/** The session is gone (an HTTP 401, the live socket's 4401): the overlay when it is on, else the redirect. */
export function goToSignIn(): void {
  if (requestSignInOverlay()) return;
  redirectToSignIn();
}

export const http: Http = createHttp({
  fetch: (input, init) => fetch(input, init),
  cookie: () => (typeof document === "undefined" ? "" : document.cookie),
  onUnauthenticated: goToSignIn,
});

/** For bodies the screens do not read (`201 {event_id}`, `204`). */
export const ignoreBody: Parser<null> = () => null;
