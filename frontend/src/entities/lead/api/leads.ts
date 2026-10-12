import { IDEMPOTENCY_HEADER, http, ignoreBody } from "@/shared/api";
import { object, str } from "@/shared/lib/parse";

import type { BookingStatus } from "../model/booking";
import type { Flow } from "../model/pricing";
import { type Channel, type LeadCard, type LeadCounts, type LeadPage, type LeadRef, type ManualChannel, type Messenger, type Stage, leadCardParser, leadCountsParser, leadPageParser, leadPath } from "../model/lead";

export interface LeadFilter {
  stage: Stage | null;
  brand: string | null;
  location: string | null;
  overdue: boolean;
  /** UTC days, `YYYY-MM-DD`, both included. */
  createdFrom: string | null;
  createdTo: string | null;
  /** Antispam's doubted leads: only them, none of them, or (null) every lead. */
  suspect: "only" | "exclude" | null;
  /** The form variant; null is every lead, those without one included. */
  flow: Flow | null;
  /** Where the lead's booking stands (`none` included); null is every lead. */
  booking: BookingStatus | null;
  /** How the lead came in; null is every lead. */
  channel: Channel | null;
  /** A messenger ref as the customer quotes it; the API matches it in any case. */
  messageRef: string | null;
}

export function fetchLeads(filter: LeadFilter, cursor: string | null, limit = 50): Promise<LeadPage> {
  return http.get("/api/v1/leads", leadPageParser, {
    stage: filter.stage,
    brand: filter.brand,
    location: filter.location,
    overdue: filter.overdue ? true : null,
    created_from: filter.createdFrom,
    created_to: filter.createdTo,
    suspect: filter.suspect,
    flow: filter.flow,
    booking: filter.booking,
    channel: filter.channel,
    message_ref: filter.messageRef,
    cursor,
    limit,
  });
}

/** Counts for the stage segments: the place filter only, so a segment counts what tapping it would list. */
export function fetchLeadCounts(filter: Pick<LeadFilter, "brand" | "location">): Promise<LeadCounts> {
  return http.get("/api/v1/leads/counts", leadCountsParser, { brand: filter.brand, location: filter.location });
}

export function fetchLeadCard(ref: LeadRef): Promise<LeadCard> {
  return http.get(leadPath(ref), leadCardParser);
}

export interface NewLead {
  brand: string;
  location: string;
  need: string;
  phone: string | null;
  /** A call (the default on the server) or a customer who wrote on a messenger without the landing. */
  channel: ManualChannel;
}

const createdParser = object({ brand: str, lead_id: str, event_id: str });

export function createLead(lead: NewLead): Promise<{ brand: string; lead_id: string }> {
  const body = { brand: lead.brand, location: lead.location, need: lead.need, channel: lead.channel, ...(lead.phone ? { phone: lead.phone } : {}) };
  return http.send("POST", "/api/v1/leads", body, createdParser);
}

/** The stage bodies `POST …/stage` takes (`panel_server::api::StageBody`). */
export type StageMove =
  | { stage: "contacted"; channel?: string }
  | { stage: "quoted"; amount?: number; currency?: string }
  | { stage: "won"; job_id?: string }
  | { stage: "lost"; reason: string; note?: string }
  | { stage: "completed" };

export async function moveLead(ref: LeadRef, move: StageMove): Promise<void> {
  await http.send("POST", `${leadPath(ref)}/stage`, move, ignoreBody);
}

/**
 * The customer wrote on a messenger (`lead.messaged`). 201; a retry under the
 * same key replays the first answer, and a lead already marked answers ok too.
 */
export async function markMessaged(ref: LeadRef, channel: Messenger, idempotencyKey: string): Promise<void> {
  await http.send("POST", `${leadPath(ref)}/messaged`, { channel }, ignoreBody, { [IDEMPOTENCY_HEADER]: idempotencyKey });
}

export interface PaymentInput {
  billed: number;
  commission: number;
  currency: string;
}

export async function recordPayment(ref: LeadRef, payment: PaymentInput): Promise<void> {
  await http.send("POST", `${leadPath(ref)}/payments`, payment, ignoreBody);
}
