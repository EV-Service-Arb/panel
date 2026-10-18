// Where the panel gets the platform's elements (sign-in, account chip) from. Opt-in at build time
// (`NEXT_PUBLIC_MFE_PLATFORM=1`): the bundles then come from the platform origin, named by its
// published registry.json (hashed file names, so no URL is written down here), and the server
// runs with PANEL_CSP_PLATFORM_MFE=1 so the CSP names those directories. Off, every caller keeps
// the same-origin URL it always had and nothing is fetched.

import { type ConciergeElement, conciergeEntry, fetchConciergeRegistry } from "@evinvest/concierge";
import type { MfeEntry, ParseResult } from "@evinvest/mfe";

/** Inlined at build time by Next; anything but "1" keeps the same-origin bundles. */
export const PLATFORM_MFE_ENABLED = process.env.NEXT_PUBLIC_MFE_PLATFORM === "1";

type LoadRegistry = (url: undefined, options: { init: RequestInit }) => Promise<ParseResult<MfeEntry[]>>;

export interface PlatformMfeOptions {
  enabled: boolean;
  /** Injected so the choice of source can be tested without a network. */
  loadRegistry?: LoadRegistry;
}

export interface PlatformMfe {
  /**
   * The URL to load `element`'s bundle from: `sameOriginUrl` when the platform source is off,
   * else the registry's entry; null when the registry cannot be had in `timeoutMs` or has no
   * valid entry, so the caller falls back the way it does for a bundle that never came.
   */
  scriptUrl(element: ConciergeElement, sameOriginUrl: string, timeoutMs: number): Promise<string | null>;
}

export function createPlatformMfe({ enabled, loadRegistry = fetchConciergeRegistry }: PlatformMfeOptions): PlatformMfe {
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
    async scriptUrl(element, sameOriginUrl, timeoutMs) {
      if (!enabled) return sameOriginUrl;
      const entries = await registry(timeoutMs);
      if (entries === null) return null;
      const entry = conciergeEntry(entries, element);
      return entry.ok ? entry.value.scriptUrl : null;
    },
  };
}

export const platformMfe = createPlatformMfe({ enabled: PLATFORM_MFE_ENABLED });
