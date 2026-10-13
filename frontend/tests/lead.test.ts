import { describe, expect, it } from "vitest";

import { dialable, slaAt } from "@/entities/lead/model/lead";

describe("the number a call dials", () => {
  it("keeps only + and digits", () => {
    expect(dialable("+33 6 00-00.00 01")).toBe("+33600000001");
    expect(dialable("06 12 34 56 78 (evenings)")).toBe("0612345678");
  });

  it("is none for fewer than six digits or no number", () => {
    expect(dialable("12-34")).toBeNull();
    expect(dialable("call me")).toBeNull();
    expect(dialable("javascript:alert(1)")).toBeNull();
    expect(dialable(null)).toBeNull();
  });
});

describe("the SLA as time passes", () => {
  const sla = { waiting_since: "2026-09-30T10:00:00Z", waiting_seconds: 60, overdue: false };
  const at = (min: number) => Date.parse("2026-09-30T10:00:00Z") + min * 60_000;

  it("counts from waiting_since, not from when the page loaded", () => {
    expect(slaAt(sla, at(20))).toEqual({ seconds: 1200, overdue: false });
  });

  it("turns overdue past 30 minutes without a reload", () => {
    expect(slaAt(sla, at(31)).overdue).toBe(true);
  });
});

describe("what else the customer left", () => {
  it("names the keys the panel knows and lists the rest as text, by key", async () => {
    const { extrasOf } = await import("@/entities/lead/model/lead");
    const got = extrasOf({ name: "Ann", need: "hot_water", bedrooms: 3, locality: "69003", zone: "<b>east</b>", extra: { floor: 2 }, blank: " " });
    expect(got.labelled).toEqual([
      { key: "locality", value: "69003" },
      { key: "bedrooms", value: "3" },
    ]);
    expect(got.other).toEqual([
      { key: "extra", value: '{"floor":2}' },
      { key: "zone", value: "<b>east</b>" },
    ]);
    expect(extrasOf(null)).toEqual({ labelled: [], other: [] });
  });
});

describe("the suspect mark", () => {
  it("is null, or one of the antispam's two words", async () => {
    const { leadParser } = await import("@/entities/lead/model/lead");
    const { parse } = await import("@/shared/lib/parse");
    const row = { brand: "a", lead_id: "l", stage: "created", manual: false, last_event_at: "t" };
    expect(parse(leadParser, row).suspect).toBeNull();
    expect(parse(leadParser, { ...row, suspect: "too_fast" }).suspect).toBe("too_fast");
    expect(() => parse(leadParser, { ...row, suspect: "maybe" })).toThrow(/suspect/);
  });
});
