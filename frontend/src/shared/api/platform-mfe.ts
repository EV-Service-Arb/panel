// Where the panel gets the platform's elements (sign-in, account chip) from: the platform origin,
// named by its published registry.json (hashed file names, so no URL is written down here). The
// server runs with PANEL_CSP_PLATFORM_MFE=1 so the CSP names those directories.

import { type ConciergeElement, conciergeEntry, fetchConciergeRegistry } from "@evinvest/concierge";
import type { MfeEntry, ParseResult } from "@evinvest/mfe";

type LoadRegistry = (url: undefined, options: { init: RequestInit }) => Promise<ParseResult<MfeEntry[]>>;

export interface PlatformMfeOptions {
  /** Injected so the choice of source can be tested without a network. */
  loadRegistry?: LoadRegistry;
}

export interface PlatformMfe {
  /**
   * The URL to load `element`'s bundle from, per the registry; null when the registry cannot be
   * had in `timeoutMs` or has no valid entry, so the caller falls back the way it does for a
   * bundle that never came.
   */
  scriptUrl(element: ConciergeElement, timeoutMs: number): Promise<string | null>;
}

export function createPlatformMfe({ loadRegistry = fetchConciergeRegistry }: PlatformMfeOptions = {}): PlatformMfe {
  // One fetch for all elements; a failed one is forgotten so the next ask retries.
  let pending: Promise<MfeEntry[] | null> | null = null;

  function registry(timeoutMs: number): Promise<MfeEntry[] | null> {
    if (pending) return pending;
    const request = loadRegistry(undefined, { init: { signal: AbortSignal.timeout(timeoutMs) } }).then(
      result => (result.ok ? result.value : null),
      () => null,
    );
    pending = request;
    void request.then(entries => {
      if (entries === null && pending === request) pending = null;
    });
    return request;
  }

  return {
    async scriptUrl(element, timeoutMs) {
      const entries = await registry(timeoutMs);
      if (entries === null) return null;
      const entry = conciergeEntry(entries, element);
      return entry.ok ? entry.value.scriptUrl : null;
    },
  };
}

export const platformMfe = createPlatformMfe();
