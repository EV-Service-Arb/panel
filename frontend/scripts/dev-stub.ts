/**
 * A stand-in for the panel's backend, for `next dev` without Rust, Postgres or
 * concierge: the `/api/v1` operator API and `/auth/*` over an in-memory set of
 * leads. Same shapes, same gate (401 without a session, CSRF on writes).
 *
 *   npm run dev:stub                  # :3121, signed in as an operator
 *   STUB_ROLE=admin npm run dev:stub  # as an admin
 *   STUB_ME=403 npm run dev:stub      # the gate refusing: 401 | 403 | 503
 *   STUB_MIN_SAMPLE=2 npm run dev:stub  # percents (and bars) from 2 leads, not 30
 *   STUB_POSTHOG=off npm run dev:stub   # no PostHog project: the funnel and experiments without a PostHog link
 *   STUB_PLACES_CONFLICT=1 npm run dev:stub  # every place-settings save answers 409
 *   STUB_PRICING_CONFLICT=1 npm run dev:stub # every pricing save and removal answers 409
 *   STUB_PRICING_INVALID=inputs.zone.labels.en npm run dev:stub  # every pricing save and preview answers 422 there
 *   STUB_LIVE=off npm run dev:stub     # no /api/v1/live socket: the panel polls instead (4401 | 4403: close at once)
 *   STUB_LIVE_EVERY=5 npm run dev:stub # live activity every 5 s rather than every 20–40 s
 *   STUB_BOOKING_CONFLICT=1 npm run dev:stub # every booking write answers 409
 *
 * Live: every write above is announced on the socket, and someone else is busy
 * too — a new lead every 20–40 s, now and then a lead moved on, a place saved,
 * vifnet's pricing saved, a landing re-declaring its experiments, a booking
 * without a lead, a provider moving a booked slot.
 *
 * then `npm run dev` in another shell. Data is made up and says so ("stub").
 */
import { randomUUID } from "node:crypto";
import { type IncomingMessage, type ServerResponse, createServer } from "node:http";
import type { Duplex } from "node:stream";

import { ALIASES } from "../src/entities/session/model/generated.ts";
import { BOOKING_STATUSES, arriveUnmatched, bookingDto, bookingWrite, bookingsRoute, providerMoves } from "./stub-bookings.ts";
import { changed, every, liveUpgrade } from "./stub-live.ts";
import { type StubDeal, dealDto, estimate, fixed, flowParam, quote, seedDeals } from "./stub-deals.ts";
import { addedPlaces, placeFlags, placeSettingsRoute, touchPlace } from "./stub-places.ts";
import { experimentsRoute } from "./stub-experiments.ts";
import { pricingRoute, touchPricing } from "./stub-pricing.ts";

const PORT = Number(process.env.STUB_PORT ?? 3121);
const ROLE = process.env.STUB_ROLE === "admin" ? "admin" : "operator";
const ME_FAILURE = process.env.STUB_ME;
const CSRF = "stub-csrf";
const MIN_SAMPLE = Number(process.env.STUB_MIN_SAMPLE ?? 30);
const LIVE_EVERY = process.env.STUB_LIVE_EVERY ? Number(process.env.STUB_LIVE_EVERY) : null;
const USER_ID = "00000000-0000-7000-8000-000000000001";
const CHANNELS = ["form", "phone_inbound", "callback", "whatsapp", "telegram"];
const MANUAL_CHANNELS = ["phone_inbound", "whatsapp", "telegram"];
const MESSAGE_REF = /^[A-Z]{2,4}-[0-9A-HJKMNP-TV-Z]{4,8}$/;

type Json = Record<string, unknown>;
const now = () => new Date();
const iso = (d: Date) => d.toISOString();
const minsAgo = (m: number) => iso(new Date(now().getTime() - m * 60_000));

interface StubLead {
  brand: string;
  lead_id: string;
  location: string | null;
  stage: string;
  manual: boolean;
  times: Record<string, string>;
  lost_reason: string | null;
  /** The landing's antispam doubted it: "rate_limited" | "too_fast". */
  suspect: string | null;
  pii: Json;
  events: Json[];
  payments: { billed: number; commission: number; currency: string }[];
  /** The form variant and its price; null on leads from before the variants. */
  deal: StubDeal | null;
  channel: string;
  /** The messenger ref the landing made (`AQ-7K3F`), and the customer's first message. */
  message_ref: string | null;
  messaged: { at: string; channel: string } | null;
}

function makeLead(i: number, stage: string, minutes: number, pii: Json, location: string | null = "lyon-3", brand = "aquafix"): StubLead {
  const created = minsAgo(minutes);
  const order = ["created", "contacted", "quoted", "won", "completed", "paid"];
  const times: Record<string, string> = { created_at: created };
  for (const s of order.slice(1, order.indexOf(stage) + 1)) times[`${s}_at`] = minsAgo(minutes - 10);
  return { brand, lead_id: `stub-${i}`, location, stage, manual: false, times, lost_reason: null, suspect: null, pii, events: [event("lead.created", { channel: "form" }, "site", created)], payments: [], deal: null, channel: "form", message_ref: null, messaged: null };
}

function event(type: string, properties: Json, kind = "panel", at = iso(now())): Json {
  return { id: randomUUID(), type, type_version: 1, occurred_at: at, received_at: at, source_kind: kind, source_id: kind === "panel" ? "panel" : "aquafix-site", manual: kind === "panel", job_id: null, status: "accepted", status_reason: null, properties, pii: null };
}

const leads: StubLead[] = [
  makeLead(1, "created", 42, { name: "Camille (stub)", phone: "+33 6 00 00 00 01", need: "Leak under the sink" }, "lyon-7"),
  makeLead(2, "created", 6, { name: "Hugo (stub)", phone: "+33 6 00 00 00 02", need: "Blocked bathroom drain" }),
  makeLead(3, "contacted", 60 * 26, { name: "Léa (stub)", phone: "+33 6 00 00 00 03", need: "Boiler not heating" }),
  makeLead(4, "quoted", 60 * 50, { name: "Noah (stub)", need: "Replace the toilet" }, "lyon-7"),
  makeLead(5, "won", 60 * 80, { phone: "+33 6 00 00 00 05", need: "Drain cleaning" }),
  makeLead(6, "paid", 60 * 200, { name: "Emma (stub)", need: "Shower repair" }, "lyon-7"),
  makeLead(7, "created", 3, { need: "Office cleaning 120 m²" }, "paris-11", "vifnet"),
  makeLead(8, "paid", 60 * 300, { name: "Jules (stub)", need: "New water heater" }),
  makeLead(9, "contacted", 60 * 30, { need: "Called without saying where" }, null),
];
// Two the antispam doubted: one sender too often, one form back too soon.
leads.push({ ...makeLead(10, "created", 12, { name: "Bot? (stub)", phone: "+33 6 00 00 00 10", need: "hot_water" }), suspect: "rate_limited" });
leads.push({ ...makeLead(11, "contacted", 60 * 5, { need: "asdf (stub)", locality: "69003", bedrooms: 2 }, "paris-11", "vifnet"), suspect: "too_fast" });
// Messenger leads: one still waiting for the customer's WhatsApp message, one who wrote on Telegram.
leads.push({ ...makeLead(12, "created", 9, { need: "Leak behind the washing machine (stub)", locality: "69003" }), channel: "whatsapp", message_ref: "AQ-7K3F" });
leads.push({ ...makeLead(13, "contacted", 60 * 3, { need: "Office cleaning, weekly (stub)" }, "paris-11", "vifnet"), channel: "telegram", message_ref: "VF-Q9MZ", messaged: { at: minsAgo(60 * 3 - 2), channel: "telegram" } });
leads[12]!.events.push(event("lead.messaged", { channel: "telegram", message_ref: "VF-Q9MZ" }, "bot", minsAgo(60 * 3 - 2)));
seedDeals(leads);
leads[5]!.payments.push({ billed: 23_100, commission: 2_310, currency: "EUR" });
leads[7]!.payments.push({ billed: 208_000, commission: 20_800, currency: "EUR" }, { billed: 9_050, commission: 0, currency: "GBP" });

const sources: Json[] = [{ key_id: "aquafix-site", kind: "site", brands: ["aquafix"], created_at: minsAgo(60 * 24 * 10), revoked_at: null }];

function leadDto(l: StubLead): Json {
  const t = (k: string) => l.times[k] ?? null;
  const waiting = l.stage === "created" && !t("contacted_at");
  const since = t("created_at") ?? iso(now());
  const secs = Math.floor((now().getTime() - Date.parse(since)) / 1000);
  return {
    brand: l.brand, lead_id: l.lead_id, location: l.location, job_id: null, stage: l.stage, channel: l.channel, manual: l.manual,
    message_ref: l.message_ref, messaged_at: l.messaged?.at ?? null, messaged_channel: l.messaged?.channel ?? null,
    created_at: t("created_at"), contacted_at: t("contacted_at"), quoted_at: t("quoted_at"), won_at: t("won_at"),
    completed_at: t("completed_at"), paid_at: t("paid_at"), lost_at: t("lost_at"), lost_reason: l.lost_reason, suspect: l.suspect,
    last_event_at: String(l.events.at(-1)?.occurred_at ?? since),
    sla: waiting ? { waiting_since: since, waiting_seconds: secs, overdue: secs > 30 * 60 } : null,
    pii: l.pii,
    ...dealDto(l.deal),
    booking: bookingDto(l.brand, l.lead_id),
  };
}

function share(n: number, of: number): Json {
  const percent = of >= MIN_SAMPLE && of > 0 ? Math.floor((n * 100 + Math.floor(of / 2)) / of) : null;
  return { n, of, percent, small_sample: percent === null };
}

/** A slice's numbers: steps, lost and manual shares, and payments summed per currency, never converted. */
function slice(ls: StubLead[]): Json {
  const has = (k: string) => ls.filter((l) => l.times[`${k}_at`]).length;
  const counts: [string, number][] = [["created", ls.length], ["contacted", has("contacted")], ["quoted", has("quoted")], ["won", has("won")], ["completed", has("completed")], ["paid", has("paid")]];
  const paid = new Map<string, { currency: string; billed: number; commission: number; count: number }>();
  for (const p of ls.flatMap((l) => l.payments)) {
    const row = paid.get(p.currency) ?? { currency: p.currency, billed: 0, commission: 0, count: 0 };
    row.billed += p.billed;
    row.commission += p.commission;
    row.count += 1;
    paid.set(p.currency, row);
  }
  return {
    stages: counts.map(([stage, n], i) => ({ stage, reached: n, of_previous: i ? share(n, counts[i - 1]![1]) : null, of_leads: share(n, ls.length) })),
    lost: share(ls.filter((l) => l.stage === "lost").length, ls.length), manual: share(ls.filter((l) => l.manual).length, ls.length),
    payments: [...paid.values()].sort((a, b) => a.currency.localeCompare(b.currency)),
  };
}

function funnel(brand: string | null, by: string | null): Json {
  const ls = leads.filter((l) => !brand || l.brand === brand);
  const head = { from: iso(new Date(now().getTime() - 29 * 86_400_000)).slice(0, 10), to: iso(now()).slice(0, 10), brand, min_sample: MIN_SAMPLE,
    posthog_url: process.env.STUB_POSTHOG === "off" ? null : "https://us.posthog.com/project/0/insights/new" };
  if (by !== "location") return { ...head, ...slice(ls) };
  const places = [...new Set(ls.map((l) => `${l.brand}/${l.location ?? ""}`))].sort();
  const locations = places.map((key) => {
    const [b, loc] = key.split("/") as [string, string];
    return { brand: b, location: loc || null, ...slice(ls.filter((l) => l.brand === b && (l.location ?? "") === loc)) };
  });
  return { ...head, by: "location", locations };
}

/** Every location a lead names, with the time of its latest lead, and those added by hand; each with its site-data flags. */
function places(): Json {
  const latest = new Map<string, { brand: string; location: string; last_lead_at: string | null }>();
  for (const p of addedPlaces()) latest.set(`${p.brand}/${p.location}`, { ...p, last_lead_at: null });
  for (const l of leads) {
    if (!l.location) continue;
    const key = `${l.brand}/${l.location}`;
    const at = l.times.created_at ?? null;
    const prev = latest.get(key);
    if (!prev || (at && (!prev.last_lead_at || at > prev.last_lead_at))) latest.set(key, { brand: l.brand, location: l.location, last_lead_at: at });
  }
  return { places: [...latest.values()].map((p) => ({ ...p, ...placeFlags(p.brand, p.location) })) };
}

/** Leads by current stage under a place filter, every stage present. */
function counts(brand: string | null, location: string | null): Json {
  const ls = leads.filter((l) => (!brand || l.brand === brand) && (!location || l.location === location));
  const stages = Object.fromEntries(["created", "contacted", "quoted", "won", "completed", "paid", "lost"].map((s) => [s, ls.filter((l) => l.stage === s).length]));
  const overdue = ls.map(leadDto).filter((l) => (l.sla as Json | null)?.overdue === true).length;
  return { stages, overdue, total: ls.length };
}

function send(res: ServerResponse, status: number, body?: unknown, headers: Record<string, string | string[]> = {}): void {
  res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", ...headers });
  res.end(body === undefined ? undefined : JSON.stringify(body));
}

async function readJson(req: IncomingMessage): Promise<Json> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  const text = Buffer.concat(chunks).toString();
  return text ? (JSON.parse(text) as Json) : {};
}

function signedIn(req: IncomingMessage): boolean {
  return /(?:^|;\s*)sa_session=/.test(req.headers.cookie ?? "");
}

/** `created_from`/`created_to` are UTC days, both included. */
function inDays(at: string, from: string | null, to: string | null): boolean {
  const day = at.slice(0, 10);
  return (!from || day >= from) && (!to || day <= to);
}

function find(brand: string, id: string): StubLead | undefined {
  return leads.find((l) => l.brand === brand && l.lead_id === id);
}

/** Never granted: the stub's role is fixed, so a request waits for good. */
const accessRequests: { need: string; requested_at: string }[] = [];

async function api(req: IncomingMessage, res: ServerResponse, url: URL): Promise<void> {
  const path = url.pathname.replace(/^\/api\/v1/, "");
  const write = req.method !== "GET";
  if (write && req.headers["x-sa-csrf"] !== CSRF) return send(res, 403, { error: "csrf" });
  if (ME_FAILURE === "403") return send(res, 403, { error: "no access to the panel" });
  if (ME_FAILURE === "503") return send(res, 503, { error: "sign-in is unavailable, try again" });
  if (ME_FAILURE === "401" || !signedIn(req)) return send(res, 401, { error: "sign in" }, { "set-cookie": "sa_session=; Path=/; Max-Age=0" });

  // The stub signs anyone in, as the backend's dev sign-in does: it says so.
  if (path === "/me") return send(res, 200, { user_id: USER_ID, permissions: ALIASES[`sa:${ROLE}`], email: "stub@example.test", preferred_name: `Stub ${ROLE}`, dev_sign_in: true });
  if (path === "/access/requests/mine") return send(res, 200, { requests: accessRequests });
  if (path === "/access/requests" && write) {
    const { need } = (await readJson(req)) as { need: string };
    const standing = accessRequests.find((r) => r.need === need);
    if (standing) return send(res, 200, standing);
    const made = { need, requested_at: new Date().toISOString() };
    accessRequests.push(made);
    return send(res, 201, made);
  }
  if (path === "/funnel") return send(res, 200, funnel(url.searchParams.get("brand"), url.searchParams.get("by")));
  if (path.startsWith("/experiments")) {
    const reply = experimentsRoute(req.method ?? "GET", path, url.searchParams, write ? await readJson(req) : {}, ROLE, `stub-${ROLE}@example.test`);
    if (reply?.changed) changed("experiments", reply.changed);
    if (reply) return send(res, reply.status, reply.body);
  }
  if (path === "/places" && req.method === "GET") return send(res, 200, places());
  if (path.startsWith("/places")) {
    const reply = placeSettingsRoute(req.method ?? "GET", path, write ? await readJson(req) : {}, ROLE, `stub-${ROLE}@example.test`);
    if (reply && write && reply.status < 300) {
      const [, , brand = null, slug = null] = path.split("/").map(decodeURIComponent);
      changed("places", brand, slug);
    }
    if (reply) return send(res, reply.status, reply.body);
  }
  if (path.startsWith("/pricing")) {
    const reply = pricingRoute(req.method ?? "GET", path, write ? await readJson(req) : {}, ROLE, `stub-${ROLE}@example.test`);
    if (reply?.changed) changed("pricing", reply.changed);
    if (reply) return send(res, reply.status, reply.body);
  }
  if (path === "/leads/counts") return send(res, 200, counts(url.searchParams.get("brand"), url.searchParams.get("location")));
  if (path === "/leads" && req.method === "GET") {
    const q = url.searchParams;
    const suspect = q.get("suspect");
    if (suspect !== null && suspect !== "only" && suspect !== "exclude") return send(res, 400, { error: "suspect is not one of only, exclude" });
    const flow = flowParam(q.get("flow"));
    if (flow === false) return send(res, 400, { error: "flow is not one of quote, estimate, fixed" });
    const booking = q.get("booking");
    if (booking !== null && !BOOKING_STATUSES.some((s) => s === booking)) return send(res, 400, { error: "booking status is not one of none, requested, booked, canceled, done, no_show" });
    const channel = q.get("channel");
    if (channel !== null && !CHANNELS.includes(channel)) return send(res, 400, { error: "channel is not one of form, phone_inbound, callback, whatsapp, telegram" });
    const ref = q.get("message_ref")?.trim().toUpperCase() ?? null;
    if (ref !== null && !MESSAGE_REF.test(ref)) return send(res, 400, { error: "message_ref is not like AQ-7K3F" });
    const list = leads
      .filter((l) => (channel === null || l.channel === channel) && (ref === null || l.message_ref === ref))
      .filter((l) => (suspect !== "only" || l.suspect !== null) && (suspect !== "exclude" || l.suspect === null))
      .filter((l) => flow === null || l.deal?.flow === flow)
      .filter((l) => booking === null || bookingDto(l.brand, l.lead_id).status === booking)
      .filter((l) => (!q.get("stage") || l.stage === q.get("stage")) && (!q.get("brand") || l.brand === q.get("brand")) && (!q.get("location") || l.location === q.get("location")))
      .map(leadDto)
      .filter((l) => q.get("overdue") !== "true" || (l.sla as Json | null)?.overdue === true)
      .filter((l) => inDays(String(l.created_at), q.get("created_from"), q.get("created_to")))
      .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)));
    return send(res, 200, { leads: list, next_cursor: null });
  }
  if (path === "/leads" && req.method === "POST") {
    const b = await readJson(req);
    const l = makeLead(leads.length + 1, "created", 0, { need: b.need, ...(b.phone ? { phone: b.phone } : {}) }, String(b.location), String(b.brand));
    l.lead_id = `p-${randomUUID()}`;
    l.manual = true;
    if (b.channel !== undefined && !MANUAL_CHANNELS.includes(String(b.channel))) return send(res, 400, { error: "channel is one of phone_inbound, whatsapp, telegram" });
    l.channel = b.channel === undefined ? "phone_inbound" : String(b.channel);
    leads.push(l);
    changed("leads", l.brand, l.lead_id);
    return send(res, 201, { brand: l.brand, lead_id: l.lead_id, event_id: randomUUID() });
  }
  if (path === "/sources" && ROLE !== "admin") return send(res, 403, { error: "your permissions do not allow this" });
  if (path === "/sources" && req.method === "GET") return send(res, 200, { sources });
  if (path === "/sources" && req.method === "POST") {
    const b = await readJson(req);
    if (sources.some((s) => s.key_id === b.key_id)) return send(res, 409, { error: `a source ${String(b.key_id)} exists already` });
    sources.push({ key_id: b.key_id, kind: b.kind, brands: b.brands, created_at: iso(now()), revoked_at: null });
    changed("sources");
    return send(res, 201, { key_id: b.key_id, secret: `stub-secret-${randomUUID()}` });
  }
  const revoke = path.match(/^\/sources\/([^/]+)$/);
  if (revoke && req.method === "DELETE") {
    const s = sources.find((x) => x.key_id === decodeURIComponent(revoke[1]!));
    if (!s) return send(res, 404, { error: "not found" });
    s.revoked_at = iso(now());
    changed("sources");
    return send(res, 204);
  }

  if (path.startsWith("/bookings/")) {
    const reply = bookingsRoute(req.method ?? "GET", path, url.searchParams, write ? await readJson(req) : {}, (brand, id) => !!find(brand, id));
    if (reply) {
      res.once("finish", () => res.statusCode < 300 && reply.told?.forEach(([topic, brand, id]) => changed(topic, brand, id)));
      return send(res, reply.status, reply.body);
    }
  }

  const m = path.match(/^\/leads\/([^/]+)\/([^/]+)(\/.*)?$/);
  const lead = m ? find(decodeURIComponent(m[1]!), decodeURIComponent(m[2]!)) : undefined;
  if (!m || !lead) return send(res, 404, { error: "not found" });
  const rest = m[3] ?? "";
  if (rest === "" && req.method === "GET") return send(res, 200, { lead: leadDto(lead), events: lead.events });
  // Every route below writes to the lead: announced once the reply is out, as after a commit.
  res.once("finish", () => res.statusCode < 300 && changed("lead", lead.brand, lead.lead_id));
  if (rest === "/booking" || rest === "/booking/status") {
    const key = req.headers["idempotency-key"];
    const reply = bookingWrite(lead.brand, lead.lead_id, rest, await readJson(req), typeof key === "string" ? key : undefined);
    return send(res, reply.status, reply.body);
  }
  if (rest === "/stage") {
    const b = await readJson(req);
    const stage = String(b.stage);
    lead.stage = stage;
    lead.times[`${stage}_at`] = iso(now());
    if (stage === "lost") lead.lost_reason = String(b.reason);
    lead.events.push(event(stage === "won" || stage === "completed" ? `job.${stage}` : `lead.${stage}`, b));
    return send(res, 201, { event_id: randomUUID() });
  }
  if (rest === "/messaged") {
    const b = await readJson(req);
    const channel = String(b.channel);
    if (channel !== "whatsapp" && channel !== "telegram") return send(res, 400, { error: "channel is one of whatsapp, telegram" });
    // The first message counts; a repeat is the same answer, nothing new journaled.
    if (!lead.messaged) {
      const e = event("lead.messaged", { channel });
      lead.messaged = { at: String(e.occurred_at), channel };
      lead.events.push(e);
    }
    return send(res, 201, { event_id: randomUUID() });
  }
  if (rest === "/calls/attempt") {
    const e = event("call.attempted", {});
    lead.events.push(e);
    return send(res, 201, { attempt_id: e.id });
  }
  if (/^\/calls\/[^/]+\/outcome$/.test(rest)) {
    const b = await readJson(req);
    lead.events.push(event("call.logged", { outcome: b.outcome }));
    return send(res, 201, { event_id: randomUUID() });
  }
  if (rest === "/payments") {
    const b = await readJson(req);
    lead.stage = "paid";
    lead.times.paid_at = iso(now());
    lead.payments.push({ billed: Number(b.billed), commission: Number(b.commission), currency: String(b.currency) });
    lead.events.push(event("payment.received", b));
    return send(res, 201, { event_id: randomUUID() });
  }
  return send(res, 404, { error: "not found" });
}

// ---- someone else at work --------------------------------------------------------------

const NEEDS = ["Dripping tap (stub)", "No hot water (stub)", "Clogged kitchen sink (stub)", "Toilet runs all night (stub)", "Window cleaning, 3 floors (stub)"];
const WHERE: [string, string][] = [["aquafix", "lyon-3"], ["aquafix", "lyon-7"], ["vifnet", "paris-11"]];
const pick = <T,>(xs: readonly T[]): T => xs[Math.floor(Math.random() * xs.length)] as T;

every(LIVE_EVERY, () => {
  const [brand, location] = pick(WHERE);
  const l = makeLead(leads.length + 1, "created", 0, { name: "Walk-in (stub)", phone: "+33 6 00 00 00 99", need: pick(NEEDS) }, location, brand);
  l.lead_id = `live-${randomUUID().slice(0, 8)}`;
  // Now and then the antispam doubts one, as a landing would mark it.
  if (Math.random() < 0.3) l.suspect = pick(["rate_limited", "too_fast"]);
  // And the sites use their form variants: vifnet prices from its model, aquafix asks for a quote.
  l.deal = brand === "vifnet" ? pick([estimate(10_450, { zone: "paris-intra", bedrooms: "1", surface: "30-50", frequency: "weekly" }), fixed(6_900)]) : pick([quote(), null]);
  leads.push(l);
  changed("leads", l.brand, l.lead_id);
});
every(LIVE_EVERY === null ? 45 : LIVE_EVERY * 2, () => {
  const waiting = leads.filter((l) => l.stage === "created");
  if (waiting.length === 0) return;
  const l = pick(waiting);
  l.stage = "contacted";
  l.times.contacted_at = iso(now());
  l.events.push(event("lead.contacted", { channel: "phone" }));
  changed("lead", l.brand, l.lead_id);
});
every(LIVE_EVERY === null ? 90 : LIVE_EVERY * 3, () => {
  touchPlace("aquafix", "lyon-3", "colleague@example.test (stub)");
  changed("places", "aquafix", "lyon-3");
});
every(LIVE_EVERY === null ? 120 : LIVE_EVERY * 4, () => changed("experiments"));
every(LIVE_EVERY === null ? 75 : LIVE_EVERY * 3, () => changed("bookings", arriveUnmatched()));
every(LIVE_EVERY === null ? 100 : LIVE_EVERY * 5, () => {
  const moved = providerMoves();
  if (moved) changed("lead", ...moved);
});
every(LIVE_EVERY === null ? 180 : LIVE_EVERY * 6, () => {
  touchPricing("colleague@example.test (stub)");
  changed("pricing", "vifnet");
});

const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (url.pathname === "/auth/login") {
    // No concierge: signing in is instant. Plain names, as the backend uses over http.
    res.writeHead(303, { location: "/", "set-cookie": ["sa_session=stub; Path=/; HttpOnly; SameSite=Lax", `sa_csrf=${CSRF}; Path=/; SameSite=Lax`] });
    return res.end();
  }
  if (url.pathname === "/auth/logout" && req.method === "POST") {
    return send(res, 204, undefined, { "set-cookie": ["sa_session=; Path=/; Max-Age=0", "sa_csrf=; Path=/; Max-Age=0"] });
  }
  if (url.pathname.startsWith("/api/v1/")) {
    api(req, res, url).catch((e: unknown) => send(res, 500, { error: String(e) }));
    return;
  }
  send(res, 404, { error: "not found" });
});
server.on("upgrade", (req: IncomingMessage, socket: Duplex) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (url.pathname !== "/api/v1/live") return socket.destroy();
  liveUpgrade(req, socket, { signedIn: signedIn(req) && ME_FAILURE !== "401", userId: USER_ID });
});
server.listen(PORT, "127.0.0.1", () => console.log(`panel dev stub on http://127.0.0.1:${PORT} as ${ROLE}`));
