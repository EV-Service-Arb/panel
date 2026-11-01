"use client";

import { useEffect, useRef } from "react";

import type { LiveStatus } from "./client";
import type { LiveMessage } from "./protocol";
import { LIVE_EVENT, LIVE_STATUS_EVENT } from "./relay";

/** The conductor's own topics (CONTRACT §3); the rest of the socket is the elements'. */
export type LiveTopic = "sources" | "telegram";

/**
 * Calls `onSignal` on a change on `topic`, if any, on a `resync`, and when the socket comes back
 * after a drop (what changed meanwhile was not told). The same window events the elements read.
 */
export function followLive(target: EventTarget, topic: LiveTopic | undefined, onSignal: () => void): () => void {
  const onMessage = (e: Event) => {
    const message = (e as CustomEvent<LiveMessage>).detail;
    if (message.type === "resync" || (message.type === "changed" && message.topic === topic)) onSignal();
  };
  let dropped = false;
  const onStatus = (e: Event) => {
    const { status } = (e as CustomEvent<{ status: LiveStatus }>).detail;
    if (status === "live") {
      if (dropped) onSignal();
      dropped = false;
    } else if (status !== "connecting") dropped = true;
  };
  target.addEventListener(LIVE_EVENT, onMessage);
  target.addEventListener(LIVE_STATUS_EVENT, onStatus);
  return () => {
    target.removeEventListener(LIVE_EVENT, onMessage);
    target.removeEventListener(LIVE_STATUS_EVENT, onStatus);
  };
}

/** `followLive` on `window`, a burst (a batch sends one change per event) folded into one call per `debounceMs`. */
export function useLiveSignal(topic: LiveTopic | undefined, onSignal: () => void, debounceMs = 400): void {
  const latest = useRef(onSignal);
  useEffect(() => {
    latest.current = onSignal;
  });

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const stop = followLive(window, topic, () => {
      timer ??= setTimeout(() => {
        timer = null;
        latest.current();
      }, debounceMs);
    });
    return () => {
      stop();
      if (timer !== null) clearTimeout(timer);
    };
  }, [topic, debounceMs]);
}
