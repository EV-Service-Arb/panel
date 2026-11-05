"use client";

import { useSyncExternalStore } from "react";

/** What the map and the outline beside it share about the GMBs. */
export interface OnMap {
  /** The located GMBs inside the map's viewport; `null` while no map is shown. */
  inView: ReadonlySet<number> | null;
  /** The GMB a pointer is on, in the outline or on the map. */
  hovered: number | null;
  /** The GMB whose card is open. */
  peeked: number | null;
}

const NONE: OnMap = { inView: null, hovered: null, peeked: null };
let state = NONE;
const listeners = new Set<() => void>();

const subscribe = (onChange: () => void) => {
  listeners.add(onChange);
  return () => listeners.delete(onChange);
};

export function useOnMap(): OnMap {
  return useSyncExternalStore(subscribe, () => state, () => NONE);
}

export function setOnMap(change: Partial<OnMap>): void {
  state = { ...state, ...change };
  for (const l of listeners) l();
}
