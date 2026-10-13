import { describe, expect, it } from "vitest";

import type { Lead } from "@/entities/lead";
import { NO_BOOKING } from "@/entities/lead/model/booking";
import { nextKnown, updatedElsewhere } from "@/views/leads/model/card-updates";
import { addWaiting, applyUpdates, arrivals, leadKey, leftFilter, pendingOf, reveal } from "@/views/leads/model/live-merge";
import { flashAttr } from "@/views/leads/ui/flash";

function lead(id: string, created: string, patch: Partial<Lead> = {}): Lead {
  return {
    brand: "aquafix",
    lead_id: id,
    location: "lyon-3",
    job_id: null,
    stage: "created",
    channel: "form",
    message_ref: null,
    messaged_at: null,
    messaged_channel: null,
    manual: false,
    created_at: `2026-10-03T${created}:00Z`,
    contacted_at: null,
    quoted_at: null,
    won_at: null,
    completed_at: null,
    paid_at: null,
    lost_at: null,
    lost_reason: null,
    suspect: null,
    last_event_at: `2026-10-03T${created}:00Z`,
    sla: null,
    pii: null,
    flow: null,
    quoted_cents: null,
    pricing_valid_from: null,
    estimate_inputs: null,
    booking: NO_BOOKING,
    ...patch,
  };
}

const keys = (ls: readonly Lead[]) => ls.map((l) => l.lead_id);
const shown = [lead("c", "10:30"), lead("b", "10:20"), lead("a", "10:10")];

describe("rows on screen under a live change", () => {
  it("are updated where they stand, and only the ones that really changed flash", () => {
    const contacted = lead("b", "10:20", { stage: "contacted", last_event_at: "2026-10-03T10:40:00Z" });
    const { leads, changed } = applyUpdates(shown, [lead("c", "10:30"), contacted]);
    expect(keys(leads)).toEqual(["c", "b", "a"]);
    expect(leads[1]?.stage).toBe("contacted");
    expect(changed).toEqual(["aquafix/b"]);
  });

  it("never gain a row from an update", () => {
    expect(keys(applyUpdates(shown, [lead("z", "11:00")]).leads)).toEqual(["c", "b", "a"]);
  });
});

describe("arrivals", () => {
  const fresh = [lead("e", "10:50"), lead("d", "10:40"), ...shown];

  it("are the fresh rows the screen does not show", () => {
    expect(keys(arrivals(shown, fresh))).toEqual(["e", "d"]);
  });

  it("gather while they wait, newest first, each once", () => {
    const waiting = addWaiting([lead("d", "10:40")], [lead("e", "10:50"), lead("d", "10:40", { stage: "contacted" })]);
    expect(keys(waiting)).toEqual(["e", "d"]);
    expect(waiting[1]?.stage).toBe("contacted");
  });

  it("count in the banner only until a reload has shown them anyway", () => {
    expect(pendingOf([lead("e", "10:50"), lead("d", "10:40")], [lead("d", "10:40"), ...shown]).map(leadKey)).toEqual(["aquafix/e"]);
  });

  it("go in by time when the person asks for them", () => {
    expect(keys(reveal(shown, [lead("d", "10:40"), lead("bb", "10:25")]))).toEqual(["d", "c", "bb", "b", "a"]);
  });
});

describe("a row that left the filter", () => {
  it("is found within a complete page", () => {
    expect(keys(leftFilter(shown, [lead("c", "10:30"), lead("a", "10:10")], true))).toEqual(["b"]);
  });

  it("is only looked for down to a full page's oldest row: older ones are simply not on it", () => {
    const page = [lead("c", "10:30"), lead("b", "10:20")];
    expect(keys(leftFilter(shown, page, false))).toEqual([]);
    expect(keys(leftFilter(shown, [lead("c", "10:30"), lead("a", "10:10")], false))).toEqual(["b"]);
  });
});

describe("the flash a row carries", () => {
  it("alternates its name so a second change replays it", () => {
    expect([flashAttr(undefined), flashAttr("new"), flashAttr(1), flashAttr(2), flashAttr(3)]).toEqual([undefined, "new", "odd", "even", "odd"]);
  });
});

describe("the open card", () => {
  const t0 = "2026-10-03T10:00:00Z";
  const t1 = "2026-10-03T10:05:00Z";
  const t2 = "2026-10-03T10:09:00Z";
  const settle = (known: Parameters<typeof nextKnown>[0], version: number, at: string | null) => nextKnown(known, version, at) ?? known;

  it("says nothing of the first read or of the person's own action", () => {
    let k = settle({ version: 0, at: null, awaiting: null }, 0, t0);
    expect(updatedElsewhere(k, 0, t0)).toBe(false);
    // They act: version 1; the old answer stays on screen, then theirs lands.
    k = settle(k, 1, t0);
    expect(updatedElsewhere(k, 1, t0)).toBe(false);
    k = settle(k, 1, t1);
    expect(updatedElsewhere(k, 1, t1)).toBe(false);
    expect(k.at).toBe(t1);
  });

  it("notes a change that arrived without the person acting", () => {
    const k = settle({ version: 0, at: null, awaiting: null }, 0, t0);
    expect(nextKnown(k, 0, t1)).toBeNull();
    expect(updatedElsewhere(k, 0, t1)).toBe(true);
  });

  it("takes the person's next action as theirs even after someone else's change", () => {
    let k = settle({ version: 0, at: null, awaiting: null }, 0, t0);
    expect(updatedElsewhere(k, 0, t1)).toBe(true);
    k = settle(k, 1, t1);
    expect(updatedElsewhere(k, 1, t1)).toBe(false);
    k = settle(k, 1, t2);
    expect(updatedElsewhere(k, 1, t2)).toBe(false);
  });
});
