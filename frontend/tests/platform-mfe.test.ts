import type { MfeEntry } from "@evinvest/mfe";
import { describe, expect, it, vi } from "vitest";

import { createPlatformMfe } from "@/shared/api/platform-mfe";

// Which URL a bundle loads from: off (the default build) the same-origin one the caller passes,
// with no request at all; on, the one concierge's registry.json names.

const PREFIX = "https://evinvest.ltd/api/auth/mfe/";
const entries: MfeEntry[] = [
  { name: "concierge.sign-in", tag: "mfe-concierge-sign-in", kind: "component", scriptUrl: `${PREFIX}sign-in.3fa9c1.js` },
  { name: "concierge.account-chip", tag: "mfe-concierge-account-chip", kind: "component", scriptUrl: `${PREFIX}account-chip.77be20.js` },
];
const SAME_ORIGIN = "/auth/mfe/sign-in.js";

describe("platform source off", () => {
  it("returns the same-origin URL and fetches nothing", async () => {
    const loadRegistry = vi.fn();
    const mfe = createPlatformMfe({ enabled: false, loadRegistry });
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).toBe(SAME_ORIGIN);
    expect(loadRegistry).not.toHaveBeenCalled();
  });
});

describe("platform source on", () => {
  it("returns the registry's hashed URL per element, from one fetch", async () => {
    const loadRegistry = vi.fn().mockResolvedValue({ ok: true, value: entries });
    const mfe = createPlatformMfe({ enabled: true, loadRegistry });
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).toBe(`${PREFIX}sign-in.3fa9c1.js`);
    expect(await mfe.scriptUrl("accountChip", "/cabinet/mfe/account-chip.js", 5000)).toBe(`${PREFIX}account-chip.77be20.js`);
    expect(loadRegistry).toHaveBeenCalledTimes(1);
  });

  it("is null when the registry has no entry for the element", async () => {
    const mfe = createPlatformMfe({ enabled: true, loadRegistry: async () => ({ ok: true, value: [entries[0]!] }) });
    expect(await mfe.scriptUrl("accountChip", "/x.js", 5000)).toBeNull();
  });

  it("is null when the registry fails, as a result or a throw, and asks again next time", async () => {
    const loadRegistry = vi
      .fn()
      .mockResolvedValueOnce({ ok: false, error: "http 503" })
      .mockRejectedValueOnce(new Error("network"))
      .mockResolvedValue({ ok: true, value: entries });
    const mfe = createPlatformMfe({ enabled: true, loadRegistry });
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).toBeNull();
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).toBeNull();
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).toBe(`${PREFIX}sign-in.3fa9c1.js`);
    expect(loadRegistry).toHaveBeenCalledTimes(3);
  });

  it("never falls back to the same-origin URL", async () => {
    const mfe = createPlatformMfe({ enabled: true, loadRegistry: async () => ({ ok: false, error: "x" }) });
    expect(await mfe.scriptUrl("signIn", SAME_ORIGIN, 5000)).not.toBe(SAME_ORIGIN);
  });
});
