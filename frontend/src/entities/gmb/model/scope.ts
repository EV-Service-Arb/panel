"use client";

import { useSyncExternalStore } from "react";

const KEY = "sa:gmail-scope";
const CHOSEN_EVENT = "sa:gmail-scope-chosen";

const subscribe = (onChange: () => void) => {
  for (const e of ["storage", CHOSEN_EVENT]) window.addEventListener(e, onChange);
  return () => {
    for (const e of ["storage", CHOSEN_EVENT]) window.removeEventListener(e, onChange);
  };
};

function current(): number | null {
  const kept = localStorage.getItem(KEY);
  if (kept === null) return null;
  const id = Number(kept);
  if (!Number.isSafeInteger(id)) throw new Error(`${KEY}: not a gmail id: ${kept}`);
  return id;
}

/** The gmail every app's GMBs are narrowed to, `null` for all of them; kept in this browser. */
export function useGmailScope(): number | null {
  return useSyncExternalStore(subscribe, current, () => null);
}

export function chooseGmailScope(gmail: number | null): void {
  if (gmail === null) localStorage.removeItem(KEY);
  else localStorage.setItem(KEY, String(gmail));
  window.dispatchEvent(new Event(CHOSEN_EVENT));
}
