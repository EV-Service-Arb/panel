import { describe, expect, it } from "vitest";

import type { LiveStatus } from "@/shared/lib/live/client";
import { relayLive } from "@/shared/lib/live/relay";
import { followLive } from "@/shared/lib/live/use-live";
import { followTitleCount } from "@/views/shell/model/title-count";

/** Every `sa:*` event `target` hears, in order, as `[type, detail]`. */
function record(target: EventTarget, types: string[]) {
  const heard: [string, unknown][] = [];
  for (const type of types) target.addEventListener(type, (e) => heard.push([type, (e as CustomEvent).detail]));
  return heard;
}

describe("the live socket on window (CONTRACT §9)", () => {
  it("relays every server message as it came, the sites' topics included", () => {
    const target = new EventTarget();
    const heard = record(target, ["sa:live"]);
    const relay = relayLive(target);
    const messages = [
      { type: "hello", at: "t", user_id: "u" },
      { type: "changed", topic: "leads", brand_id: "aquafix", id: "L-1", at: "t" },
      { type: "resync" },
      { type: "something-new", n: 1 },
    ];
    for (const m of messages) relay.message(m);
    expect(heard).toEqual(messages.map((m) => ["sa:live", m]));
  });

  it("tells the status on each change, and again to an element that asks", () => {
    const target = new EventTarget();
    const heard = record(target, ["sa:live-status"]);
    const relay = relayLive(target);
    relay.status("connecting");
    relay.status("live");
    target.dispatchEvent(new CustomEvent("sa:live-status-request"));
    relay.stop();
    target.dispatchEvent(new CustomEvent("sa:live-status-request"));
    expect(heard).toEqual([
      ["sa:live-status", { status: "connecting" }],
      ["sa:live-status", { status: "live" }],
      ["sa:live-status", { status: "live" }],
    ]);
  });

  it("re-reads a conductor screen on its topic, a resync, and the socket back after a drop; not on the sites' topics", () => {
    const target = new EventTarget();
    const relay = relayLive(target);
    let reads = 0;
    const stop = followLive(target, "sources", () => (reads += 1));
    const step = (s: LiveStatus) => relay.status(s);
    step("connecting");
    step("live");
    expect(reads).toBe(0);
    relay.message({ type: "changed", topic: "leads", at: "t" });
    expect(reads).toBe(0);
    relay.message({ type: "changed", topic: "sources", at: "t" });
    relay.message({ type: "resync" });
    expect(reads).toBe(2);
    step("reconnecting");
    step("offline");
    step("live");
    expect(reads).toBe(3);
    stop();
    relay.message({ type: "resync" });
    expect(reads).toBe(3);
  });
});

describe("the tab title (sa:title-count)", () => {
  const count = (target: EventTarget, detail: unknown) => target.dispatchEvent(new CustomEvent("sa:title-count", { detail }));

  it("counts new leads ahead of the title while above zero, and gives the title back", () => {
    const target = new EventTarget();
    const doc = { title: "Service-Arb panel" };
    const stop = followTitleCount(target, doc, "Leads");
    count(target, { count: 3 });
    expect(doc.title).toBe("(3) Leads — Service-Arb panel");
    count(target, { count: 4 });
    expect(doc.title).toBe("(4) Leads — Service-Arb panel");
    count(target, { count: 0 });
    expect(doc.title).toBe("Service-Arb panel");
    count(target, { count: 2 });
    stop();
    expect(doc.title).toBe("Service-Arb panel");
    count(target, { count: 5 });
    expect(doc.title).toBe("Service-Arb panel");
  });

  it("refuses anything but a count", () => {
    const doc = { title: "Service-Arb panel" };
    // A listener's throw does not reach dispatchEvent's caller: the test holds the listener itself.
    let listener: (e: Event) => void = () => {};
    const target = { addEventListener: (_: string, l: (e: Event) => void) => void (listener = l), removeEventListener: () => {} };
    followTitleCount(target as unknown as EventTarget, doc, "Leads");
    for (const detail of [{ count: -1 }, { count: 1.5 }, { count: "3" }, {}, null]) {
      expect(() => listener(new CustomEvent("sa:title-count", { detail }))).toThrow();
    }
    expect(doc.title).toBe("Service-Arb panel");
  });
});
