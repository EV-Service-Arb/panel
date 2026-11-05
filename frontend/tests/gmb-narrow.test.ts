import { describe, expect, it } from "vitest";

import type { Gmail, Gmb } from "@/entities/gmb/model/gmb";
import { byView, narrowed } from "@/entities/gmb/model/narrow";

const gmb = (id: number, name: string, address: string | null, located: boolean): Gmb => ({
  id,
  place_id: `ChIJplace${id}`,
  name,
  address,
  gbp_account: null,
  gbp_location: null,
  created_at: "2026-11-05T00:00:00Z",
  enabled: true,
  location: located ? { lat: 45.76, lng: 4.83 } : null,
});

const gmails: Gmail[] = [
  { id: 1, gmail: "eric@aquafix.top", created_at: "2026-11-05T00:00:00Z", gmbs: [gmb(10, "Aquafix · Vaise", "Lyon 9e", true), gmb(11, "Aquafix · Gerland", "Lyon 7e", false)] },
  { id: 2, gmail: "sameday.ny", created_at: "2026-11-05T00:00:00Z", gmbs: [gmb(20, "Same Day Plumbing", null, true)] },
];

const ids = (listed: { gmb: Gmb }[]) => listed.map((l) => l.gmb.id);

describe("the outline's GMBs", () => {
  it("are the scoped gmail's, matched on name, address, place id or gmail", () => {
    for (const [scope, filter, want] of [
      [null, "", [10, 11, 20]],
      [2, "", [20]],
      [null, "lyon 7E", [11]],
      [null, "ChIJplace20", [20]],
      [null, "aquafix.top", [10, 11]],
      [1, "same day", []],
    ] as const) {
      expect(ids(narrowed(gmails, scope, filter)), `${scope} ${filter}`).toEqual(want);
    }
  });

  it("split by the viewport put the unlocated elsewhere", () => {
    const { inView, elsewhere } = byView(narrowed(gmails, null, ""), new Set([10]));
    expect([ids(inView), ids(elsewhere)]).toEqual([[10], [11, 20]]);
  });
});
