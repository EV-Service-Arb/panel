import { type Infer, nullable, object, oneOf, parse, str } from "@/shared/lib/parse";

/** Same origin as the pages, so the session cookie rides along; the server checks `Origin`. */
export const LIVE_PATH = "/api/v1/live";

export const LIVE_TOPICS = ["leads", "lead", "places", "sources", "experiments", "telegram", "pricing", "bookings", "invoices"] as const;
export type LiveTopic = (typeof LIVE_TOPICS)[number];

/** The session is gone: the browser goes to sign-in, as on an HTTP 401. */
export const CLOSE_UNAUTHENTICATED = 4401;
/** The grant on the panel was withdrawn: reconnecting would only be refused again. */
export const CLOSE_FORBIDDEN = 4403;

const changedParser = object({ topic: oneOf(LIVE_TOPICS), brand_id: nullable(str), id: nullable(str), at: str });
/** Sent after the change has committed, so a read made on it sees the change. */
export type ChangedEvent = Infer<typeof changedParser>;

const helloParser = object({ at: str, user_id: str });

export type LiveMessage = ({ type: "hello" } & Infer<typeof helloParser>) | ({ type: "changed" } & ChangedEvent) | { type: "resync" };

/**
 * A frame from the server, or null for anything the panel does not read — a
 * topic added on the server before the front knows it, say. Ignoring it is
 * safe: the next `resync` (or reconnect) re-reads everything anyway.
 */
export function parseLiveMessage(data: unknown): LiveMessage | null {
  if (typeof data !== "string") return null;
  let body: unknown;
  try {
    body = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof body !== "object" || body === null || !("type" in body)) return null;
  try {
    switch (body.type) {
      case "hello":
        return { type: "hello", ...parse(helloParser, body) };
      case "changed":
        return { type: "changed", ...parse(changedParser, body) };
      case "resync":
        return { type: "resync" };
      default:
        return null;
    }
  } catch {
    return null;
  }
}
