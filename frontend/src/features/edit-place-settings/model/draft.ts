import { type EditedFields, MESSENGER_SWITCHES, type MessengerSwitch, type Messengers, type PlaceSettings, messengerOn } from "@/entities/place";

import { type BookingDraft, bookingDraftOf, bookingDraftProblems, bookingOf } from "./booking-draft";
import { type HoursDraftRow, hoursDraftOf, hoursOf, hoursValid } from "./hours-draft";

/** The form's state: text as typed. An empty field means "the site's own". */
export interface SettingsDraft {
  phone: string;
  whatsapp: string;
  /** The bot's username as typed; a leading `@` is dropped on save. */
  telegram: string;
  /** Every switch spelled out: on unless the place turned it off. */
  messengers: Record<MessengerSwitch, boolean>;
  hours: HoursDraftRow[];
  serviceArea: string[];
  booking: BookingDraft;
  reviewUrl: string;
}

export function draftOf(edited: EditedFields): SettingsDraft {
  return {
    phone: edited.phone ?? "",
    whatsapp: edited.whatsapp ?? "",
    telegram: edited.telegram ?? "",
    messengers: { whatsapp: messengerOn(edited, "whatsapp"), telegram: messengerOn(edited, "telegram") },
    hours: hoursDraftOf(edited.hours),
    serviceArea: [...(edited.serviceArea ?? [])],
    booking: bookingDraftOf(edited.booking),
    reviewUrl: edited.reviewUrl ?? "",
  };
}

/** "+33 6 12-34.56 (78)" → "+33612345678": what people paste, as E.164 has it. */
export function normalisePhone(raw: string): string {
  return raw.replace(/[\s().-]/g, "");
}

const E164 = /^\+[1-9]\d{6,14}$/;

/**
 * A soft check: the server's rule decides (its 422 is shown on the field), this
 * only warns while typing. Empty is fine — the site keeps its own number.
 */
export function looksLikeE164(raw: string): boolean {
  const s = normalisePhone(raw);
  return s === "" || E164.test(s);
}

/** "@aquafix_devis_bot " → "aquafix_devis_bot": the username as the server stores it. */
export function normaliseBot(raw: string): string {
  return raw.trim().replace(/^@/, "");
}

/** `panel_core::place`'s rule for a Telegram username. */
const BOT = /^[A-Za-z][A-Za-z0-9_]{1,28}[Bb][Oo][Tt]$/;

/** Like `looksLikeE164`: a hint while typing, the server's 422 decides. Empty is fine — no bot. */
export function looksLikeBot(raw: string): boolean {
  const s = normaliseBot(raw);
  return s === "" || BOT.test(s);
}

/** The hosts `panel_core::place` takes a review link on. */
const REVIEW_HOSTS = ["g.page", "search.google.com"];

/** Like `looksLikeBot`: a hint while typing, the server's 422 says why. Empty is fine — the site keeps its own link. */
export function looksLikeReviewUrl(raw: string): boolean {
  const s = raw.trim();
  if (s === "") return true;
  // Plain ASCII only, and a fragment is not a page: `https://g.page/#x` names nothing.
  if ([...s].some((c) => c.charCodeAt(0) > 127)) return false;
  const m = /^https:\/\/([^/?#]*)([^#]*)/i.exec(s);
  return m !== null && REVIEW_HOSTS.includes((m[1] ?? "").toLowerCase()) && /[^/?]/.test(m[2] ?? "");
}

/** Only the switches turned off are stored: absent is on, so an all-on place keeps no `messengers` at all. */
function messengersOf(draft: Record<MessengerSwitch, boolean>): Messengers | null {
  const off: Messengers = {};
  for (const m of MESSENGER_SWITCHES) if (!draft[m]) off[m] = false;
  return Object.keys(off).length > 0 ? off : null;
}

/** A commune joins the list once, compared without case or surrounding space. */
export function addArea(list: readonly string[], raw: string): string[] {
  const name = raw.trim().replace(/\s+/g, " ");
  if (!name || list.some((n) => n.toLocaleLowerCase() === name.toLocaleLowerCase())) return [...list];
  return [...list, name];
}

export function removeArea(list: readonly string[], name: string): string[] {
  return list.filter((n) => n !== name);
}

export function editedOf(draft: SettingsDraft): EditedFields {
  const out: EditedFields = {};
  const phone = normalisePhone(draft.phone);
  const whatsapp = normalisePhone(draft.whatsapp);
  const telegram = normaliseBot(draft.telegram);
  const messengers = messengersOf(draft.messengers);
  const hours = hoursOf(draft.hours);
  if (phone) out.phone = phone;
  if (whatsapp) out.whatsapp = whatsapp;
  if (telegram) out.telegram = telegram;
  if (messengers) out.messengers = messengers;
  if (hours) out.hours = hours;
  if (draft.serviceArea.length > 0) out.serviceArea = [...draft.serviceArea];
  const booking = bookingOf(draft.booking);
  if (booking) out.booking = booking;
  const reviewUrl = draft.reviewUrl.trim();
  if (reviewUrl) out.reviewUrl = reviewUrl;
  return out;
}

/** The settings to PUT: the draft's fields over everything the form does not edit, as it came. */
export function settingsOf(draft: SettingsDraft, base: PlaceSettings): PlaceSettings {
  return { edited: editedOf(draft), rest: base.rest };
}

export function draftValid(draft: SettingsDraft): boolean {
  return hoursValid(draft.hours) && bookingDraftProblems(draft.booking).size === 0;
}

export function draftChanged(draft: SettingsDraft, base: PlaceSettings): boolean {
  return JSON.stringify(editedOf(draft)) !== JSON.stringify(normalisedEdited(base.edited));
}

/** The stored fields in the form's own key order, so an untouched form compares equal. */
function normalisedEdited(e: EditedFields): EditedFields {
  return editedOf(draftOf(e));
}
