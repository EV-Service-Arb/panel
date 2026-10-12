import { describe, expect, it } from "vitest";

import { channelPreview } from "@/entities/place/lib/channels";
import { diffLineText, diffSettings } from "@/entities/place/lib/diff";
import { formatDays, formatHours, isOpenAt, localTimeOf } from "@/entities/place/lib/hours";
import { placesParser } from "@/entities/place/model/place";
import { type HoursRow, type PlaceSettings, historyParser, placeSettingsParser, settingsParser, settingsToWire } from "@/entities/place/model/settings";
import { addArea, draftChanged, draftOf, editedOf, looksLikeE164, looksLikeReviewUrl, settingsOf } from "@/features/edit-place-settings/model/draft";
import { fieldErrorsOf } from "@/features/edit-place-settings/model/field-errors";
import { addHoursRow, hoursDraftOf, hoursOf, normaliseTime, removeHoursRow, rowNotes, rowProblems, updateHoursRow } from "@/features/edit-place-settings/model/hours-draft";
import { createHttp } from "@/shared/api/http";
import { translator } from "@/shared/i18n/translate";
import { parse } from "@/shared/lib/parse";

const en = translator("en");

const weekdays: HoursRow = { days: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"], opens: "08:00", closes: "19:00" };
const address = { street: "12 rue Paul Bert", postalCode: "69003", locality: "Lyon" };

const settings = (o: unknown): PlaceSettings => parse(settingsParser, o);

describe("the settings answer", () => {
  const view = {
    brand: "aquafix",
    slug: "royat",
    withdrawn: false,
    settings: { phone: "+33423500640", hours: [weekdays], serviceArea: ["Royat", "Chamalières"], address, rating: { value: 4.8, count: 31, fetchedAt: "2026-10-01T00:00:00Z" } },
    updated_at: "2026-10-03T12:00:00Z",
    updated_by: "admin@evinvest.ltd",
    can_edit: true,
  };

  it("splits the fields the form edits from those it carries through", () => {
    const v = parse(placeSettingsParser, view);
    expect(v.settings.edited).toEqual({ phone: "+33423500640", hours: [weekdays], serviceArea: ["Royat", "Chamalières"] });
    expect(Object.keys(v.settings.rest).sort()).toEqual(["address", "rating"]);
  });

  it("sends back the fields it does not edit as they came", () => {
    const v = parse(placeSettingsParser, view);
    const draft = draftOf(v.settings.edited);
    draft.phone = "+33 6 12 34 56 78";
    expect(settingsToWire(settingsOf(draft, v.settings))).toEqual({ ...view.settings, phone: "+33612345678" });
  });

  it("reads an empty settings object and a null updated_at, a place never set", () => {
    const v = parse(placeSettingsParser, { ...view, settings: {}, updated_at: null, updated_by: null });
    expect(v.settings).toEqual({ edited: {}, rest: {} });
    expect(v.updated_at).toBeNull();
  });

  it("refuses a day that is not a weekday name", () => {
    expect(() => parse(placeSettingsParser, { ...view, settings: { hours: [{ days: ["Mo"], opens: "08:00", closes: "19:00" }] } })).toThrow(/hours\[0\]\.days\[0\]/);
  });

  it("reads the history, newest first as given", () => {
    const h = parse(historyParser, { changes: [{ id: "c2", at: "2026-10-03T12:00:00Z", by: "cli", before: {}, after: { phone: "+33612345678" } }] });
    expect(h.changes[0]?.after.edited.phone).toBe("+33612345678");
  });

  it("reads /places from before the flags as neither set nor withdrawn", () => {
    const p = parse(placesParser, { places: [{ brand: "aquafix", location: "royat", last_lead_at: null }, { brand: "aquafix", location: "lyon-3", last_lead_at: null, has_settings: true, withdrawn: true }] });
    expect(p.places.map((x) => [x.has_settings, x.withdrawn])).toEqual([
      [false, false],
      [true, true],
    ]);
  });
});

describe("the hours editor", () => {
  const rows = hoursDraftOf([weekdays, { days: ["Saturday"], opens: "09:00", closes: "12:00" }]);

  it("round-trips the stored rows, days back in week order", () => {
    const shuffled = updateHoursRow(rows, rows[0]?.key ?? -1, { days: ["Friday", "Monday", "Wednesday", "Tuesday", "Thursday"] });
    expect(hoursOf(shuffled)).toEqual([weekdays, { days: ["Saturday"], opens: "09:00", closes: "12:00" }]);
  });

  it("offers a new row the days no row has", () => {
    expect(addHoursRow(rows).at(-1)?.days).toEqual(["Sunday"]);
  });

  it("means the site's own hours once every row is gone", () => {
    const none = rows.reduce((acc, r) => removeHoursRow(acc, r.key), rows);
    expect(hoursOf(none)).toBeUndefined();
  });

  it("takes the times people type", () => {
    expect(["8:00", "0800", "8h30", "08.15", " 9:05 "].map(normaliseTime)).toEqual(["08:00", "08:00", "08:30", "08:15", "09:05"]);
    expect(normaliseTime("25:00")).toBe("25:00");
  });

  it("names what stops a row from saving", () => {
    const [row] = hoursDraftOf([{ days: [], opens: "8", closes: "08:00" }]);
    if (!row) throw new Error("no row");
    expect(rowProblems([row], row).map((p) => p.kind)).toEqual(["no_days", "bad_opens"]);
    const same = { ...row, days: ["Monday" as const], opens: "08:00" };
    expect(rowProblems([same], same).map((p) => p.kind)).toEqual(["same_time"]);
  });

  it("allows a lunch break, refuses rows whose times overlap on a day", () => {
    const split = hoursDraftOf([
      { days: ["Monday"], opens: "08:00", closes: "12:00" },
      { days: ["Monday", "Tuesday"], opens: "14:00", closes: "19:00" },
    ]);
    expect(split.flatMap((r) => rowProblems(split, r))).toEqual([]);
    const clash = updateHoursRow(split, split[1]?.key ?? -1, { opens: "11:00" });
    expect(rowProblems(clash, clash[0] ?? split[0]!)).toEqual([{ kind: "overlap", days: ["Monday"] }]);
  });

  it("notes a row past midnight, and counts it on the next day", () => {
    const night = hoursDraftOf([
      { days: ["Friday"], opens: "20:00", closes: "02:00" },
      { days: ["Saturday"], opens: "01:00", closes: "05:00" },
    ]);
    expect(rowNotes(night[0]!)).toEqual(["overnight"]);
    expect(rowProblems(night, night[1]!)).toEqual([{ kind: "overlap", days: ["Saturday"] }]);
  });
});

describe("the form draft", () => {
  it("is unchanged until a field is", () => {
    const base = settings({ phone: "+33612345678", hours: [{ days: ["Tuesday", "Monday"], opens: "08:00", closes: "19:00" }], address });
    const draft = draftOf(base.edited);
    expect(draftChanged(draft, base)).toBe(false);
    expect(draftChanged({ ...draft, whatsapp: "+33600000000" }, base)).toBe(true);
  });

  it("drops an emptied field, so the site's own value comes back", () => {
    const draft = draftOf({ phone: "+33612345678", serviceArea: ["Royat"] });
    expect(editedOf({ ...draft, phone: "  ", serviceArea: [] })).toEqual({});
  });

  it("warns on a number that is not E.164, never on an empty one", () => {
    expect(["+33 6 12 34 56 78", "+33612345678", ""].map(looksLikeE164)).toEqual([true, true, true]);
    expect(["0612345678", "+0612345678", "+33"].map(looksLikeE164)).toEqual([false, false, false]);
  });

  it("adds a commune once, whatever its case or spacing", () => {
    expect(addArea(["Royat"], "  royat ")).toEqual(["Royat"]);
    expect(addArea(["Royat"], " Chamalières  sur  Loire ")).toEqual(["Royat", "Chamalières sur Loire"]);
    expect(addArea(["Royat"], "   ")).toEqual(["Royat"]);
  });
});

describe("a 422", () => {
  it("is told field by field, a path inside a field on that field", async () => {
    const http = createHttp({
      fetch: async () => new Response(JSON.stringify({ error: "invalid", fields: { phone: "must be E.164", "hours[0].opens": "must be HH:MM", geo: "out of range" } }), { status: 422 }),
      cookie: () => "",
      onUnauthenticated: () => {},
    });
    const err: unknown = await http.send("PUT", "/x", {}, (v) => v).catch((e: unknown) => e);
    const failure = (err as { failure?: unknown }).failure;
    expect(failure).toEqual({ kind: "invalid_fields", fields: { phone: "must be E.164", "hours[0].opens": "must be HH:MM", geo: "out of range" } });
    const sorted = fieldErrorsOf({ phone: "must be E.164", "hours[0].opens": "must be HH:MM", geo: "out of range" });
    expect(sorted.byField).toEqual({ phone: ["must be E.164"], hours: ["must be HH:MM"] });
    expect(sorted.other).toEqual([{ key: "geo", reason: "out of range" }]);
  });

  it("without fields stays a plain failure", async () => {
    const http = createHttp({ fetch: async () => new Response("{}", { status: 422 }), cookie: () => "", onUnauthenticated: () => {} });
    const err: unknown = await http.get("/x", (v) => v).catch((e: unknown) => e);
    expect((err as { failure?: unknown }).failure).toEqual({ kind: "failed", status: 422, message: "" });
  });
});

describe("the change history in plain words", () => {
  it("says what was set, changed and removed, and nothing about what stayed", () => {
    const before = settings({ phone: "+33100000000", hours: [weekdays], address });
    const after = settings({ phone: "+33200000000", whatsapp: "+33600000000", address });
    expect(diffSettings(before, after, en).map((l) => diffLineText(l, en))).toEqual([
      "Phone: +33100000000 → +33200000000",
      "WhatsApp: set to +33600000000",
      "Opening hours: removed (was Mon–Fri 08:00–19:00); the site's own again",
    ]);
  });

  it("names a field the panel does not edit by its wire name", () => {
    const lines = diffSettings(settings({ address }), settings({}), en);
    expect(lines).toEqual([{ field: "address", before: JSON.stringify(address), after: null }]);
  });

  it("finds no difference between equal settings", () => {
    expect(diffSettings(settings({ serviceArea: ["Royat"] }), settings({ serviceArea: ["Royat"] }), en)).toEqual([]);
  });

  it("writes days as runs, short runs one by one", () => {
    expect(formatDays(["Saturday", "Monday", "Tuesday", "Wednesday"], en)).toBe("Mon–Wed, Sat");
    expect(formatDays(["Monday", "Tuesday", "Sunday"], en)).toBe("Mon, Tue, Sun");
    expect(formatHours([weekdays, { days: ["Saturday"], opens: "09:00", closes: "12:00" }], translator("ru"))).toBe("Пн–Пт 08:00–19:00; Сб 09:00–12:00");
  });
});

describe("the channel preview", () => {
  const hours = [weekdays];
  const fields = { phone: "+33423500640", whatsapp: "+33612345678", hours };
  const order = (p: ReturnType<typeof channelPreview>) => p.slots.map((s) => s.channel);

  it("puts the call first while open", () => {
    const p = channelPreview(fields, { day: "Monday", minutes: 10 * 60 });
    expect(p.state).toBe("open");
    expect(order(p)).toEqual(["call", "whatsapp", "callback"]);
  });

  it("puts the callback and WhatsApp first while closed, the call last", () => {
    const p = channelPreview(fields, { day: "Monday", minutes: 19 * 60 });
    expect(p.state).toBe("closed");
    expect(order(p)).toEqual(["callback", "whatsapp", "call"]);
    expect(channelPreview(fields, { day: "Sunday", minutes: 10 * 60 }).state).toBe("closed");
  });

  it("does not guess open or closed on the site's own hours", () => {
    expect(channelPreview({ phone: "+33423500640" }, { day: "Monday", minutes: 3 * 60 }).state).toBe("unknown");
  });

  it("never fills in a number the panel does not have", () => {
    const p = channelPreview({ hours }, { day: "Monday", minutes: 10 * 60 });
    expect(p.slots.find((s) => s.channel === "call")).toEqual({ channel: "call", value: null, from: "site" });
    expect(p.slots.find((s) => s.channel === "whatsapp")).toEqual({ channel: "whatsapp", value: null, from: "site" });
  });

  it("reads hours past midnight as open into the next morning", () => {
    const night = [{ days: ["Friday" as const], opens: "20:00", closes: "02:00" }];
    expect(isOpenAt(night, { day: "Saturday", minutes: 60 })).toBe(true);
    expect(isOpenAt(night, { day: "Saturday", minutes: 3 * 60 })).toBe(false);
  });

  it("tells the time on the place's clock, not the browser's", () => {
    // 2026-10-05 is a Monday; 06:30 UTC is 08:30 in Paris (summer time).
    expect(localTimeOf(new Date("2026-10-05T06:30:00Z"), "Europe/Paris")).toEqual({ day: "Monday", minutes: 8 * 60 + 30 });
    expect(localTimeOf(new Date("2026-10-04T23:30:00Z"), "Europe/Paris")).toEqual({ day: "Monday", minutes: 90 });
  });
});

describe("the review link", () => {
  const short = "https://g.page/r/CabcDEF123/review";

  it("is an edited field: read, trimmed on save, and shown in the history", () => {
    const base = settings({ reviewUrl: short, address });
    expect(base.edited.reviewUrl).toBe(short);
    const draft = draftOf(base.edited);
    expect(draftChanged(draft, base)).toBe(false);
    draft.reviewUrl = ` ${short}x `;
    expect(settingsToWire(settingsOf(draft, base))).toEqual({ address, reviewUrl: `${short}x` });
    expect(editedOf({ ...draft, reviewUrl: "  " })).toEqual({});
    const lines = diffSettings(settings({}), base, en);
    expect(diffLineText(lines[0]!, en)).toBe(`Google review link: set to ${short}`);
  });

  it("warns while typing about what the server would refuse", () => {
    for (const ok of ["", short, "https://search.google.com/local/writereview?placeid=ChIJ", "https://G.PAGE/x"]) expect(looksLikeReviewUrl(ok), ok).toBe(true);
    for (const bad of ["http://g.page/x", "javascript:alert(1)", "https://evil.example/x", "https://g.page", "https://g.page@evil.example/x", "https://g.page:81/x", "https://g.page/#x", "https://search.google.com/#anything", "https://g.page/r/x\u200b", "https://g.page/r/\u202ex"]) expect(looksLikeReviewUrl(bad), bad).toBe(false);
  });

  it("puts the server's reason beside its field", () => {
    expect(fieldErrorsOf({ reviewUrl: "must not have a port" }).byField.reviewUrl).toEqual(["must not have a port"]);
  });
});
