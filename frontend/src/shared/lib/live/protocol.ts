/** Same origin as the pages, so the session cookie rides along; the server checks `Origin`. */
export const LIVE_PATH = "/api/v1/live";

/** The session is gone: the browser goes to sign-in, as on an HTTP 401. */
export const CLOSE_UNAUTHENTICATED = 4401;
/** The grant on the panel was withdrawn: reconnecting would only be refused again. */
export const CLOSE_FORBIDDEN = 4403;

/** A server frame as it came: relayed to the elements untouched, whose topics the conductor does not know. */
export type LiveMessage = { type: string } & Record<string, unknown>;

export function parseLiveMessage(data: unknown): LiveMessage {
  if (typeof data !== "string") throw new Error("live: a binary frame");
  const body: unknown = JSON.parse(data);
  if (typeof body !== "object" || body === null || !("type" in body) || typeof body.type !== "string") {
    throw new Error(`live: a frame without a type: ${data}`);
  }
  return body as LiveMessage;
}
