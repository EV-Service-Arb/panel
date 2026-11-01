import { backoffDelay } from "./backoff";
import { CLOSE_FORBIDDEN, CLOSE_UNAUTHENTICATED, type LiveMessage, parseLiveMessage } from "./protocol";

/**
 * - `connecting` — the first attempt;
 * - `live` — a socket is open;
 * - `reconnecting` — it dropped (or never opened) and the next attempt is scheduled;
 * - `offline` — several attempts in a row never opened, while the attempts go on;
 * - `paused` — the tab is hidden and no socket is open: nothing is attempted;
 * - `closed` — stopped, or the server ended the session for good.
 */
export type LiveStatus = "connecting" | "live" | "reconnecting" | "offline" | "paused" | "closed";

export interface SocketEvents {
  open(): void;
  message(data: unknown): void;
  close(code: number): void;
}

/** Opens a socket that reports through `on`; a refused upgrade (429, 404) is a close without an open. */
export type Connect = (url: string, on: SocketEvents) => { close(code?: number): void };

interface Timers {
  set(fn: () => void, ms: number): number;
  clear(id: number): void;
}

export interface LiveClientOptions {
  url: string;
  connect: Connect;
  timers: Timers;
  random: () => number;
  /** Attempts in a row that never open before the client calls itself offline. */
  failuresBeforeOffline?: number;
}

export interface LiveListener {
  status(status: LiveStatus): void;
  message(message: LiveMessage): void;
  unauthenticated(): void;
  forbidden(): void;
}

export interface LiveClient {
  start(): void;
  stop(): void;
  /** The tab went hidden. */
  pause(): void;
  /** The tab is visible again. */
  resume(): void;
  /** The network is back (`online`): try now rather than at the next backoff step. */
  nudge(): void;
}

/**
 * One socket per tab, kept up, reconnecting with exponential backoff and jitter. What was missed
 * while it was down is the readers' to re-read, on the status going `live` again.
 *
 * A hidden tab keeps an open socket — the tab title counts new leads exactly
 * while the person looks elsewhere — but makes no attempts while it has none;
 * becoming visible reconnects at once.
 */
export function createLiveClient(options: LiveClientOptions, on: LiveListener): LiveClient {
  const threshold = options.failuresBeforeOffline ?? 3;
  const { timers } = options;

  let status: LiveStatus = "closed";
  let socket: { close(code?: number): void } | null = null;
  // Each socket's handlers check this, so a socket closed on purpose cannot
  // report back into a newer one's state.
  let generation = 0;
  let attempt = 0;
  let failures = 0;
  let hidden = false;
  let ended = false;
  let retry: number | null = null;

  const set = (next: LiveStatus) => {
    if (status === next) return;
    status = next;
    on.status(next);
  };
  const clearRetry = () => {
    if (retry !== null) timers.clear(retry);
    retry = null;
  };
  const drop = () => {
    generation += 1;
    socket?.close(1000);
    socket = null;
  };
  const end = (final: LiveStatus) => {
    ended = true;
    clearRetry();
    drop();
    set(final);
  };

  function connect() {
    clearRetry();
    drop();
    const mine = ++generation;
    let opened = false;
    const ours = () => mine === generation && !ended;
    socket = options.connect(options.url, {
      open() {
        if (!ours()) return;
        opened = true;
        attempt = 0;
        failures = 0;
        set("live");
      },
      message(data) {
        if (ours()) on.message(parseLiveMessage(data));
      },
      close(code) {
        if (!ours()) return;
        socket = null;
        if (code === CLOSE_UNAUTHENTICATED || code === CLOSE_FORBIDDEN) {
          end("closed");
          if (code === CLOSE_UNAUTHENTICATED) on.unauthenticated();
          else on.forbidden();
          return;
        }
        if (!opened) failures += 1;
        if (hidden) return set("paused");
        set(failures >= threshold ? "offline" : "reconnecting");
        retry = timers.set(connect, backoffDelay(attempt, options.random));
        attempt += 1;
      },
    });
  }

  return {
    start() {
      if (ended || status !== "closed") return;
      set("connecting");
      connect();
    },
    stop() {
      if (!ended) end("closed");
    },
    pause() {
      hidden = true;
      if (ended || status === "live") return;
      clearRetry();
      drop();
      set("paused");
    },
    resume() {
      hidden = false;
      if (ended || status !== "paused") return;
      attempt = 0;
      set(failures >= threshold ? "offline" : "reconnecting");
      connect();
    },
    nudge() {
      if (ended || hidden || (status !== "reconnecting" && status !== "offline")) return;
      attempt = 0;
      connect();
    },
  };
}
