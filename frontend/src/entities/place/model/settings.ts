import { MESSENGERS, type Messenger } from "@/entities/lead";
import { type Infer, type Parser, arrayOf, bool, nullable, object, oneOf, record, str } from "@/shared/lib/parse";

import { type BookingConfig, bookingConfigOf } from "./booking";

/** kitstart's `DayOfWeek`: the wire names, Monday first as the site lists them. */
export const DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"] as const;
export type Day = (typeof DAYS)[number];

const hoursRowParser = object({ days: arrayOf(oneOf(DAYS)), opens: str, closes: str });
export type HoursRow = Infer<typeof hoursRowParser>;

/** The landing's messengers the panel can switch off: the lead's own list, as generated from `panel_core`. */
export const MESSENGER_SWITCHES = MESSENGERS;
export type MessengerSwitch = Messenger;

/** The landing's messenger buttons, switched off where `false`; a key absent is on. */
export type Messengers = Partial<Record<MessengerSwitch, boolean>>;

/** The kill switches as stored, or null for anything else (a key the server would now refuse): it rides in `rest`. */
function messengersOf(v: unknown): Messengers | null {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return null;
  const out: Messengers = {};
  for (const [k, on] of Object.entries(v)) {
    const key = MESSENGER_SWITCHES.find((m) => m === k);
    if (!key || typeof on !== "boolean") return null;
    out[key] = on;
  }
  return out;
}

/**
 * The fields the panel edits in v1. Everything else a place's live data holds
 * (address, geo, storefrontPhoto, landmark, rating) stays in `rest`, untouched,
 * and goes back as it came: a PUT is a full replace.
 */
export interface EditedFields {
  phone?: string;
  whatsapp?: string;
  /** The place's Telegram bot: its username, without the `@`. */
  telegram?: string;
  messengers?: Messengers;
  hours?: HoursRow[];
  serviceArea?: string[];
  /** The booking providers the place offers and its default. */
  booking?: BookingConfig;
  /** Where a customer leaves a Google review: an https link on g.page or search.google.com. */
  reviewUrl?: string;
}

export const EDITED_KEYS = ["phone", "whatsapp", "telegram", "messengers", "hours", "serviceArea", "booking", "reviewUrl"] as const satisfies readonly (keyof EditedFields)[];

export interface PlaceSettings {
  edited: EditedFields;
  rest: Record<string, unknown>;
}

/** The live data as stored, split into what the form edits and what it carries through. */
export const settingsParser: Parser<PlaceSettings> = (v, path) => {
  const o = record(v, path);
  const edited: EditedFields = {};
  if (o.phone !== undefined) edited.phone = str(o.phone, `${path}.phone`);
  if (o.whatsapp !== undefined) edited.whatsapp = str(o.whatsapp, `${path}.whatsapp`);
  if (o.telegram !== undefined) edited.telegram = str(o.telegram, `${path}.telegram`);
  const messengers = o.messengers === undefined ? null : messengersOf(o.messengers);
  if (messengers) edited.messengers = messengers;
  if (o.hours !== undefined) edited.hours = arrayOf(hoursRowParser)(o.hours, `${path}.hours`);
  if (o.serviceArea !== undefined) edited.serviceArea = arrayOf(str)(o.serviceArea, `${path}.serviceArea`);
  // A booking the server would now refuse (its rules grew) is not the form's to edit: it rides in `rest`.
  const booking = o.booking === undefined ? null : bookingConfigOf(o.booking);
  if (booking) edited.booking = booking;
  if (o.reviewUrl !== undefined) edited.reviewUrl = str(o.reviewUrl, `${path}.reviewUrl`);
  const rest = Object.fromEntries(Object.entries(o).filter(([k]) => !(k in edited)));
  return { edited, rest };
};

/** The wire object of a PUT: what the form says over what it does not edit. */
export function settingsToWire(s: PlaceSettings): Record<string, unknown> {
  return { ...s.rest, ...s.edited };
}

/** `GET /places/{brand}/{slug}/settings`, and every write's answer. */
export const placeSettingsParser = object({
  brand: str,
  slug: str,
  withdrawn: bool,
  settings: settingsParser,
  updated_at: nullable(str),
  updated_by: nullable(str),
  can_edit: bool,
});
export type PlaceSettingsView = Infer<typeof placeSettingsParser>;

const changeParser = object({ id: str, at: str, by: str, before: settingsParser, after: settingsParser });
export type SettingsChange = Infer<typeof changeParser>;

/** `GET …/settings/history`, newest first. */
export const historyParser = object({ changes: arrayOf(changeParser) });
