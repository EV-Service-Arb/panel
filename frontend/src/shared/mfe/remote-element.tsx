"use client";

// Mounts a remote's custom element imperatively once its bundle defined it (not JSX: React's
// attribute handling differs per framework's element). Light DOM only. The bundle runs in this
// origin unsandboxed: its URL is code, never user input.

import { useEffect, useRef, type ReactNode } from "react";

import { useRemoteBundle } from "./use-remote-bundle";

// An element is created once per configuration and re-inserted after: a remote that cannot be torn down
// (a Dioxus one, ev_lib's `mfe`) reuses its running app on a re-insert, and mounting it anew would stack a second.
const created = new Map<string, HTMLElement>();

export interface RemoteElementProps {
  tag: string;
  scriptUrl: string;
  attributes: Record<string, string>;
  className?: string | undefined;
  /** Until the element is defined, and if the bundle fails to load. */
  fallback?: ReactNode | undefined;
}

export function RemoteElement({ tag, scriptUrl, attributes, className, fallback = null }: RemoteElementProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const attributesRef = useRef(attributes);
  const ready = useRemoteBundle(tag, scriptUrl);

  useEffect(() => {
    attributesRef.current = attributes;
  });

  // Keyed on [ready, tag] only: the remote survives prop churn.
  useEffect(() => {
    const host = hostRef.current;
    if (!host || !ready) return;
    // the language is the remote's configuration too: it reads it off the DOM it mounts into
    const key = JSON.stringify([tag, attributesRef.current, host.closest("[lang]")?.getAttribute("lang") ?? null]);
    let element = created.get(key);
    if (element === undefined) {
      element = document.createElement(tag);
      // Before appendChild: the upgrade runs connectedCallback synchronously, where a remote
      // reads its configuration.
      for (const [name, value] of Object.entries(attributesRef.current)) element.setAttribute(name, value);
      created.set(key, element);
    }
    host.appendChild(element);
    return () => element.remove();
  }, [ready, tag]);

  return (
    <div ref={hostRef} className={className}>
      {ready ? null : fallback}
    </div>
  );
}
