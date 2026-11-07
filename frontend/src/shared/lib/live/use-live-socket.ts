"use client";

import { useEffect, useRef, useState } from "react";

import { type Connect, type LiveStatus, createLiveClient } from "./client";
import { LIVE_PATH } from "./protocol";
import { relayLive } from "./relay";

const browserConnect: Connect = (url, on) => {
  const ws = new WebSocket(url);
  ws.onopen = () => on.open();
  ws.onmessage = (e: MessageEvent) => on.message(e.data);
  ws.onclose = (e: CloseEvent) => on.close(e.code);
  return { close: (code) => ws.close(code) };
};

export interface LiveSocketHandlers {
  /** Close 4401: the session ended. */
  onUnauthenticated: () => void;
  /** Close 4403: the person lost access to the panel. */
  onForbidden: () => void;
}

/**
 * The tab's one socket, relayed on `window` for the conductor's screens and the elements alike;
 * mounted once by the shell. Hidden and visible map to the client's pause and resume, `online` to
 * a retry now.
 */
export function useLiveSocket(handlers: LiveSocketHandlers): LiveStatus {
  const latest = useRef(handlers);
  const [status, setStatus] = useState<LiveStatus>("closed");
  useEffect(() => {
    latest.current = handlers;
  });

  useEffect(() => {
    const relay = relayLive(window);
    // `ws:`/`wss:` on the page's own host: the cookie and the `Origin` check need the same origin.
    const url = `${window.location.protocol === "https:" ? "wss:" : "ws:"}//${window.location.host}${LIVE_PATH}`;
    const client = createLiveClient(
      {
        url,
        connect: browserConnect,
        timers: { set: (fn, ms) => window.setTimeout(fn, ms), clear: (id) => window.clearTimeout(id) },
        random: Math.random,
      },
      {
        status: (s) => {
          relay.status(s);
          setStatus(s);
        },
        message: relay.message,
        unauthenticated: () => latest.current.onUnauthenticated(),
        forbidden: () => latest.current.onForbidden(),
      },
    );
    const onVisibility = () => (document.visibilityState === "hidden" ? client.pause() : client.resume());
    const onOnline = () => client.nudge();
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("online", onOnline);
    client.start();
    if (document.visibilityState === "hidden") client.pause();
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("online", onOnline);
      client.stop();
      relay.stop();
    };
  }, []);
  return status;
}
