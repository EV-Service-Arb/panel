import type { Invoice, InvoiceState } from "../model/invoice";

/** The shape of an id that the wallet element and its action page accept (banking `actionId`). */
const ACTION_ID = /^[A-Za-z0-9_-]{1,128}$/;

export type StateTone = "positive" | "pending" | "neutral" | "ended";

/** How a state reads in the list. */
export function stateTone(state: InvoiceState): StateTone {
  switch (state) {
    case "paid":
      return "positive";
    case "creating":
    case "processing":
      return "pending";
    case "open":
      return "neutral";
    case "cancelled":
    case "expired":
    case "unknown":
      return "ended";
  }
}

/**
 * The banking invoice id to put on the pay button, or null when the row is not payable now.
 * Only an `open` row is: `processing` already has a payment in flight, and a final row takes
 * none. Paid is decided by the row alone, never by what the pay popup reported.
 */
export function payableInvoiceId(invoice: Invoice): string | null {
  if (invoice.state !== "open" || invoice.invoice_id === null) return null;
  return ACTION_ID.test(invoice.invoice_id) ? invoice.invoice_id : null;
}
