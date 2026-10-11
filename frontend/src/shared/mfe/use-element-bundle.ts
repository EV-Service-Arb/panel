import { useEffect, useState } from "react";

import { containRemoteStyles } from "./contain-remote-styles";

/** `loading` until the bundle answers; `failed` is final for this mount (the caller keeps its fallback). */
export type BundleState = "loading" | "ready" | "failed";

/** The slice of `customElements` the wait needs; a parameter so it runs without a DOM. */
export interface ElementRegistry {
  get(tag: string): CustomElementConstructor | undefined;
  whenDefined(tag: string): Promise<CustomElementConstructor>;
}

/**
 * Whether a defined element speaks at least `min` of its host contract. A bundle from before
 * the contract has no static `apiVersion`, and rendering it with the new attributes would
 * half-work (an older chip reads the cabinet session the host does not have).
 */
export function speaksApi(ctor: unknown, min: number): boolean {
  if (typeof ctor !== "function") return false;
  const version: unknown = (ctor as { apiVersion?: unknown }).apiVersion;
  return typeof version === "number" && version >= min;
}

/**
 * Settles true once `tag` is defined by a bundle speaking `minApiVersion`; false on `failure`
 * (the script did not load), on `timeoutMs`, or on an older bundle. Never rejects.
 */
export function awaitElement(
  registry: ElementRegistry,
  tag: string,
  minApiVersion: number,
  timeoutMs: number,
  failure: Promise<void>,
): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<false>((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  const defined = registry.whenDefined(tag).then(
    (ctor) => speaksApi(ctor, minApiVersion),
    () => false,
  );
  return Promise.race([defined, timeout, failure.then(() => false)]).finally(() => clearTimeout(timer));
}

export interface ElementBundleOptions {
  minApiVersion: number;
  /** A bundle that neither loads nor fails (a hung proxy) must not hold the fallback forever. */
  timeoutMs: number;
}

// One load per tag across mounts (and React's dev double effect): a failed one is forgotten so
// the next mount retries, a good one is shared.
const loads = new Map<string, Promise<boolean>>();

function loadOnce(tag: string, scriptUrl: string, { minApiVersion, timeoutMs }: ElementBundleOptions): Promise<boolean> {
  const pending = loads.get(tag);
  if (pending) return pending;
  let failure: Promise<void> = new Promise<void>(() => {});
  if (!customElements.get(tag)) {
    const script = document.createElement("script");
    script.type = "module";
    script.src = scriptUrl;
    failure = new Promise<void>((resolve) => {
      script.addEventListener(
        "error",
        () => {
          script.remove();
          resolve();
        },
        { once: true },
      );
    });
    document.head.appendChild(script);
  }
  const load = awaitElement(customElements, tag, minApiVersion, timeoutMs, failure);
  loads.set(tag, load);
  void load.then((ok) => {
    if (!ok) loads.delete(tag);
  });
  return load;
}

/**
 * Loads a remote's self-registering ESM bundle once per tag and says whether its element is
 * usable. Unlike `useRemoteBundle` it gives up: a 404 (served as JSON, so a MIME error), an
 * older bundle and a timeout all read as `failed`, never as an endless `loading`.
 */
export function useElementBundle(tag: string, scriptUrl: string, { minApiVersion, timeoutMs }: ElementBundleOptions): BundleState {
  const [state, setState] = useState<{ tag: string; value: BundleState } | null>(null);

  useEffect(() => {
    let cancelled = false;
    const stopContainment = containRemoteStyles(scriptUrl);
    void loadOnce(tag, scriptUrl, { minApiVersion, timeoutMs }).then((ok) => {
      if (!cancelled) setState({ tag, value: ok ? "ready" : "failed" });
    });
    return () => {
      cancelled = true;
      stopContainment();
    };
  }, [tag, scriptUrl, minApiVersion, timeoutMs]);

  return state?.tag === tag ? state.value : "loading";
}
