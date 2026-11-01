import type { LiveStatus } from "./client";
import type { LiveMessage } from "./protocol";

// CONTRACT §9: the elements' only view of the socket, all on `window`.
export const LIVE_EVENT = "sa:live";
export const LIVE_STATUS_EVENT = "sa:live-status";
const LIVE_STATUS_REQUEST_EVENT = "sa:live-status-request";

export interface LiveRelay {
  message(message: LiveMessage): void;
  status(status: LiveStatus): void;
  stop(): void;
}

/** Re-dispatches the socket on `target`; an element mounted after a status change asks for it again. */
export function relayLive(target: EventTarget): LiveRelay {
  let current: LiveStatus = "closed";
  const tell = () => target.dispatchEvent(new CustomEvent(LIVE_STATUS_EVENT, { detail: { status: current } }));
  target.addEventListener(LIVE_STATUS_REQUEST_EVENT, tell);
  return {
    message: (message) => target.dispatchEvent(new CustomEvent(LIVE_EVENT, { detail: message })),
    status(status) {
      current = status;
      tell();
    },
    stop: () => target.removeEventListener(LIVE_STATUS_REQUEST_EVENT, tell),
  };
}
