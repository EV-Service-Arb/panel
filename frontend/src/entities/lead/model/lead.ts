import { type Infer, arrayOf, bool, cents, dictOf, isoDay, nullable, num, object, oneOf, oneOfOr, record, recordOf, str } from "@/shared/lib/parse";

import { leadBookingParser } from "./booking";
import { CHANNELS, CONTACT_SLA_SECONDS, FLOWS, MESSENGERS, type Channel, type Messenger, STAGES, SUSPECTS, type Stage } from "./generated";

export { CHANNELS, CONTACT_SLA_SECONDS, MANUAL_CHANNELS, MESSENGERS, STAGES, SUSPECTS } from "./generated";
export type { Channel, ManualChannel, Messenger, Stage, Suspect } from "./generated";

/** A lead's channel as read: one the panel knows, or `other` for one a newer backend added. */
export type LeadChannel = Channel | "other";

const slaParser = object({ waiting_since: str, waiting_seconds: num, overdue: bool });

export const leadParser = object({
  brand: str,
  lead_id: str,
  location: nullable(str),
  job_id: nullable(str),
  stage: oneOf(STAGES),
  /** How the customer reached the brand; null for a lead seen before its creation, `other` for a channel newer than this build. */
  channel: nullable(oneOfOr(CHANNELS, "other")),
  /** The ref the landing gave the customer to quote in a messenger (`AQ-7K3F`). */
  message_ref: nullable(str),
  /** When the customer first wrote on a messenger, and on which (null too for a messenger newer than this build). */
  messaged_at: nullable(str),
  messaged_channel: nullable(oneOfOr(MESSENGERS, null)),
  manual: bool,
  created_at: nullable(str),
  contacted_at: nullable(str),
  quoted_at: nullable(str),
  won_at: nullable(str),
  completed_at: nullable(str),
  paid_at: nullable(str),
  lost_at: nullable(str),
  lost_reason: nullable(str),
  /** Kept for good once marked: progress does not clear it. */
  suspect: nullable(oneOf(SUSPECTS)),
  last_event_at: str,
  /** Set while the lead waits for its first contact; overdue after 30 minutes. */
  sla: nullable(slaParser),
  /** What the customer left, for the roles that see it. */
  pii: nullable(record),
  /** The form variant the site used; the three fields below are absent from older leads. */
  flow: nullable(oneOf(FLOWS)),
  /** EUR TTC in integer cents, with `estimate` and `fixed` only. */
  quoted_cents: nullable(cents),
  /** The day the price model used took effect. */
  pricing_valid_from: nullable(isoDay),
  /** `estimate` only: input id → value id, as the site's model names them (no labels yet). */
  estimate_inputs: nullable(dictOf(str)),
  /** Where the lead's booking stands: `status: none` when it has none. */
  booking: leadBookingParser,
});
export type Lead = Infer<typeof leadParser>;

export const leadEventParser = object({
  id: str,
  type: str,
  type_version: num,
  occurred_at: str,
  received_at: str,
  source_kind: str,
  source_id: str,
  manual: bool,
  job_id: nullable(str),
  status: str,
  status_reason: nullable(str),
  properties: record,
  pii: nullable(record),
});
export type LeadEvent = Infer<typeof leadEventParser>;

export const leadPageParser = object({ leads: arrayOf(leadParser), next_cursor: nullable(str) });
export type LeadPage = Infer<typeof leadPageParser>;

export const leadCardParser = object({ lead: leadParser, events: arrayOf(leadEventParser) });
export type LeadCard = Infer<typeof leadCardParser>;

/** `GET /leads/counts`: leads by their current stage (every stage, 0 included), and how many are overdue. */
export const leadCountsParser = object({ stages: recordOf(STAGES, num), overdue: num, total: num });
export type LeadCounts = Infer<typeof leadCountsParser>;

/**
 * The wait as of `now`, from `waiting_since` — the API's `waiting_seconds` and
 * `overdue` are as of the answer, and a list left open must keep counting.
 */
export function slaAt(sla: NonNullable<Lead["sla"]>, now: number): { seconds: number; overdue: boolean } {
  const since = Date.parse(sla.waiting_since);
  const seconds = Number.isNaN(since) ? sla.waiting_seconds : Math.max(sla.waiting_seconds, Math.floor((now - since) / 1000));
  return { seconds, overdue: sla.overdue || seconds > CONTACT_SLA_SECONDS };
}

/** The messenger a lead waits on: one it came through, the customer not having written yet. */
export function awaitingMessage(lead: Pick<Lead, "channel" | "messaged_at">): Messenger | null {
  if (lead.messaged_at !== null) return null;
  return MESSENGERS.find((m) => m === lead.channel) ?? null;
}

/** A lead's address in the API and in the page URL: `brand/lead`. */
export interface LeadRef {
  brand: string;
  lead: string;
}

export function refOf(lead: Pick<Lead, "brand" | "lead_id">): LeadRef {
  return { brand: lead.brand, lead: lead.lead_id };
}

export function encodeRef(ref: LeadRef): string {
  return `${ref.brand}/${ref.lead}`;
}

export function decodeRef(raw: string | null): LeadRef | null {
  if (!raw) return null;
  const at = raw.indexOf("/");
  if (at <= 0 || at === raw.length - 1) return null;
  return { brand: raw.slice(0, at), lead: raw.slice(at + 1) };
}

export function leadPath(ref: LeadRef): string {
  return `/api/v1/leads/${encodeURIComponent(ref.brand)}/${encodeURIComponent(ref.lead)}`;
}

/** What the customer left, the known fields picked out as text. */
export interface Contact {
  name: string | null;
  phone: string | null;
  email: string | null;
  need: string | null;
}

export function contactOf(pii: Record<string, unknown> | null): Contact {
  const text = (k: string) => {
    const v = pii?.[k];
    return typeof v === "string" && v.trim() !== "" ? v : null;
  };
  return { name: text("name"), phone: text("phone"), email: text("email"), need: text("need") };
}

/** Keys a site sends that have a label of their own; `need` may be a site's raw id (`hot_water`) and shows as sent. */
export const LABELLED_PII = ["locality", "bedrooms"] as const;
export type LabelledPii = (typeof LABELLED_PII)[number];

const CONTACT_KEYS: readonly string[] = ["name", "phone", "email", "need"];

/** A value the customer left, as plain text: never markup — it is their input. */
function asText(v: unknown): string | null {
  if (v === null || v === undefined) return null;
  if (typeof v === "string") return v.trim() === "" ? null : v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}

/**
 * Everything else the customer left: the labelled keys first, in their order,
 * then the rest by key — a site may send fields the panel has no name for yet.
 */
export function extrasOf(pii: Record<string, unknown> | null): { labelled: { key: LabelledPii; value: string }[]; other: { key: string; value: string }[] } {
  const labelled = LABELLED_PII.flatMap((key) => {
    const value = asText(pii?.[key]);
    return value === null ? [] : [{ key, value }];
  });
  const known: readonly string[] = [...CONTACT_KEYS, ...LABELLED_PII];
  const other = Object.entries(pii ?? {})
    .filter(([k]) => !known.includes(k))
    .flatMap(([key, v]) => {
      const value = asText(v);
      return value === null ? [] : [{ key, value }];
    })
    .sort((a, b) => a.key.localeCompare(b.key));
  return { labelled, other };
}

/**
 * The number as a `tel:` link may carry it: `+` and digits only, whatever the
 * customer typed around them. Fewer than six digits is not a number to dial —
 * the page shows the text and offers no call.
 */
export function dialable(phone: string | null): string | null {
  if (!phone) return null;
  const tel = phone.replace(/[^\d+]/g, "");
  return tel.replace(/\D/g, "").length >= 6 ? tel : null;
}

/**
 * Whether a lead reached a stage, by the stage times the projection keeps (a
 * lead lost after a quote did reach "quoted"). The same rule the backend's
 * funnel counts by.
 */
export function reached(lead: Lead, stage: Exclude<Stage, "lost">): boolean {
  switch (stage) {
    case "created":
      return true;
    case "contacted":
      return lead.contacted_at !== null || reached(lead, "quoted");
    case "quoted":
      return lead.quoted_at !== null || reached(lead, "won");
    case "won":
      return lead.won_at !== null || reached(lead, "completed");
    case "completed":
      return lead.completed_at !== null || reached(lead, "paid");
    case "paid":
      return lead.paid_at !== null;
  }
}
