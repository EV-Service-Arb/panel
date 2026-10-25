// The bounds `panel_core::invoice` holds an invoice to. The form checks them so that the
// operator sees the problem before the request; the backend checks them again.

/** Whole USDT up to 999 999 999. */
export const MAX_INTEGER_DIGITS = 9;
/** A micro-USDT. */
export const MAX_FRACTION_DIGITS = 6;
/** Banking counts the memo in characters (code points), not in bytes or UTF-16 units. */
export const MAX_MEMO_CHARS = 140;
export const MIN_EXPIRES_IN_DAYS = 1;
export const MAX_EXPIRES_IN_DAYS = 30;

/**
 * `raw` as banking's canonical decimal (`12.5`, `0.5`, `1000`), or null when the backend would
 * refuse it. Same rules as `canonical_amount`, plus one comma as the decimal point: a Russian
 * keyboard types `12,5`, and the backend takes only a point.
 */
export function canonicalAmount(raw: string): string | null {
  const text = raw.trim().replace(",", ".");
  if (!/^\d*(\.\d*)?$/.test(text)) return null;
  const [whole = "", fraction = ""] = text.split(".");
  if (whole === "" && fraction === "") return null;
  const int = whole.replace(/^0+/, "");
  const frac = fraction.replace(/0+$/, "");
  if (int.length > MAX_INTEGER_DIGITS || frac.length > MAX_FRACTION_DIGITS) return null;
  // Zero, however it is written, is not an amount to bill.
  if (int === "" && frac === "") return null;
  return `${int === "" ? "0" : int}${frac === "" ? "" : `.${frac}`}`;
}

/** The memo the payer reads: within the length, on one line, without control characters. */
export function memoProblem(memo: string): "too_long" | "control" | null {
  if ([...memo].length > MAX_MEMO_CHARS) return "too_long";
  if (/[\u0000-\u001F\u007F-\u009F]/.test(memo)) return "control";
  return null;
}

/** Whole days from 1 to 30, or null. */
export function expiresInDays(raw: string): number | null {
  const text = raw.trim();
  if (!/^\d{1,2}$/.test(text)) return null;
  const days = Number(text);
  return days >= MIN_EXPIRES_IN_DAYS && days <= MAX_EXPIRES_IN_DAYS ? days : null;
}
