import type { T } from "@/shared/i18n";

import { EDITED_KEYS, type EditedFields, MESSENGER_SWITCHES, type PlaceSettings } from "../model/settings";
import { bookingText } from "./booking";
import { formatHours } from "./hours";

export interface DiffLine {
  /** One of the edited keys, or a carried-through one such as `address`. */
  field: string;
  /** Plain text, or null for "not set: the site's own". */
  before: string | null;
  after: string | null;
}

type EditedKey = (typeof EDITED_KEYS)[number];
const isEdited = (k: string): k is EditedKey => (EDITED_KEYS as readonly string[]).includes(k);

function editedText(key: EditedKey, fields: EditedFields, t: T): string | null {
  switch (key) {
    case "phone":
    case "whatsapp":
      return fields[key] ?? null;
    case "telegram":
      return fields.telegram ? `@${fields.telegram}` : null;
    case "messengers":
      return messengersText(fields, t);
    case "hours":
      return fields.hours ? formatHours(fields.hours, t) : null;
    case "serviceArea":
      return fields.serviceArea ? fields.serviceArea.join(", ") : null;
    case "booking":
      return fields.booking ? bookingText(fields.booking, t) : null;
  }
}

/** "WhatsApp, Telegram off"; null while every messenger is on, as with no switches at all. */
export function messengersText(fields: Pick<EditedFields, "messengers">, t: T): string | null {
  const off = MESSENGER_SWITCHES.filter((m) => fields.messengers?.[m] === false);
  return off.length === 0 ? null : t("placeSettings.messengers.off", { names: off.map((m) => t(`placeSettings.preview.${m}`)).join(", ") });
}

/** A field the panel does not edit, shown as its JSON: it is rare and read by an admin. */
const restText = (v: unknown): string | null => (v === undefined ? null : JSON.stringify(v));

/** The fields a change touched, edited ones first in form order, each before and after. */
export function diffSettings(before: PlaceSettings, after: PlaceSettings, t: T): DiffLine[] {
  const rest = [...new Set([...Object.keys(before.rest), ...Object.keys(after.rest)])].filter((k) => !isEdited(k)).sort();
  const lines: DiffLine[] = [
    ...EDITED_KEYS.map((field) => ({ field, before: editedText(field, before.edited, t), after: editedText(field, after.edited, t) })),
    ...rest.map((field) => ({ field, before: restText(before.rest[field]), after: restText(after.rest[field]) })),
  ];
  return lines.filter((l) => l.before !== l.after);
}

/** The field's name in the reader's language; one the panel does not know keeps its wire name. */
export function fieldLabel(field: string, t: T): string {
  return isEdited(field) ? t(`placeSettings.field.${field}`) : field;
}

/** One line in plain words: "Phone: +33 1 → +33 2", "WhatsApp: set to +33…", "Hours: removed". */
export function diffLineText(line: DiffLine, t: T): string {
  const label = fieldLabel(line.field, t);
  if (line.before === null) return t("placeSettings.diff.set", { field: label, after: line.after ?? "" });
  if (line.after === null) return t("placeSettings.diff.removed", { field: label, before: line.before });
  return t("placeSettings.diff.changed", { field: label, before: line.before, after: line.after });
}
