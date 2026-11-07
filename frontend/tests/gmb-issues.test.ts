import { describe, expect, it } from "vitest";

import type { ArchiveTag, Gmb, SiteTag } from "@/entities/gmb/model/gmb";
import { type Issue, type Tags, issueCounts, issueOf } from "@/entities/gmb/model/issues";

const gmb = (id: number, { located = true, enabled = true } = {}): Gmb => ({
  id,
  place_id: `ChIJplace${id}`,
  name: `GMB ${id}`,
  address: null,
  gbp_account: null,
  gbp_location: null,
  created_at: "2026-11-06T00:00:00Z",
  enabled,
  location: located ? { lat: 45.76, lng: 4.83, placed: false } : null,
});
const archive = (suspended: number, failing: boolean): ArchiveTag => ({ tracked: true, rating: 4.5, reviews: 10, suspended, reinstating: 0, failing });
const site = (overdue: number): SiteTag => ({ brand: "aquafix", location: "lyon-9", leads_7d: 3, overdue });

/** Each GMB's issue under `tags`, by id. */
function check(gmbs: Gmb[], tags: Tags, want: Record<number, Issue | null>) {
  expect(Object.fromEntries(gmbs.map((g) => [g.id, issueOf(g, tags)]))).toEqual(want);
}

describe("a GMB's issue", () => {
  const every = [gmb(1, { located: false }), gmb(2), gmb(3), gmb(4), gmb(5, { located: false, enabled: false })];

  it("is the most pressing one it has; an off one's is only that it is off", () => {
    check(
      every,
      {
        archive: new Map([
          [1, archive(2, true)],
          [2, archive(0, true)],
          [3, archive(0, false)],
          [4, archive(0, false)],
          [5, archive(2, true)],
        ]),
        sites: new Map([
          [1, site(1)],
          [2, site(0)],
          [3, site(0)],
          [5, site(1)],
        ]),
      },
      { 1: "overdue", 2: "failing", 3: null, 4: "no_site", 5: "off" },
    );
  });

  it("drops what a service that did not answer would have said", () => {
    const tags: Tags = { archive: null, sites: null };
    check(every, tags, { 1: "unlocated", 2: null, 3: null, 4: null, 5: "off" });
    expect(issueCounts(every, tags)).toEqual(
      new Map<Issue, number>([
        ["unlocated", 1],
        ["off", 1],
      ]),
    );
  });
});
