import type { MfeEntry } from "@evinvest/mfe";
import { describe, expect, it, vi } from "vitest";

import { createPlatformMfe } from "@/shared/api/platform-mfe";

// Which URL a bundle loads from: the one concierge's registry.json names, or null.

const PREFIX = "https://evinvest.ltd/api/auth/mfe/";
const entries: MfeEntry[] = [
  { name: "concierge.sign-in", tag: "mfe-concierge-sign-in", kind: "component", scriptUrl: `${PREFIX}sign-in.3fa9c1.js` },
  { name: "concierge.account-chip", tag: "mfe-concierge-account-chip", kind: "component", scriptUrl: `${PREFIX}account-chip.77be20.js` },
];

describe("platform source", () => {
  it("returns the registry's hashed URL per element, from one fetch", async () => {
    const loadRegistry = vi.fn().mockResolvedValue({ ok: true, value: entries });
    const mfe = createPlatformMfe({ loadRegistry });
    expect(await mfe.scriptUrl("signIn", 5000)).toBe(`${PREFIX}sign-in.3fa9c1.js`);
    expect(await mfe.scriptUrl("accountChip", 5000)).toBe(`${PREFIX}account-chip.77be20.js`);
    expect(loadRegistry).toHaveBeenCalledTimes(1);
  });

  it("is null when the registry has no entry for the element", async () => {
    const mfe = createPlatformMfe({ loadRegistry: async () => ({ ok: true, value: [entries[0]!] }) });
    expect(await mfe.scriptUrl("accountChip", 5000)).toBeNull();
  });

  it("is null when the registry fails, as a result or a throw, and asks again next time", async () => {
    const loadRegistry = vi
      .fn()
      .mockResolvedValueOnce({ ok: false, error: "http 503" })
      .mockRejectedValueOnce(new Error("network"))
      .mockResolvedValue({ ok: true, value: entries });
    const mfe = createPlatformMfe({ loadRegistry });
    expect(await mfe.scriptUrl("signIn", 5000)).toBeNull();
    expect(await mfe.scriptUrl("signIn", 5000)).toBeNull();
    expect(await mfe.scriptUrl("signIn", 5000)).toBe(`${PREFIX}sign-in.3fa9c1.js`);
    expect(loadRegistry).toHaveBeenCalledTimes(3);
  });
});
