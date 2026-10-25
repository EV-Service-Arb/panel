import { type Infer, arrayOf, nullable, object, oneOfOr, str, webUrl } from "@/shared/lib/parse";

/** Where an invoice stands, as the panel stores it (`panel_core::invoice::InvoiceState`). */
export const INVOICE_STATES = ["creating", "open", "processing", "paid", "cancelled", "expired"] as const;
export type KnownInvoiceState = (typeof INVOICE_STATES)[number];

/**
 * A state that the backend added before this build knows it reads as `unknown`, so one row
 * does not fail the whole list. It is never payable and never paid.
 */
export type InvoiceState = KnownInvoiceState | "unknown";

/** A row of `GET /api/v1/invoices` (`panel_server::invoices::InvoiceDto`). */
export const invoiceParser = object({
  id: str,
  /** Banking's id; null until banking answered the create. */
  invoice_id: nullable(str),
  /** A canonical decimal as a string: a JSON number would round it. */
  amount: str,
  currency: str,
  memo: str,
  state: oneOfOr(INVOICE_STATES, "unknown"),
  pay_url: nullable(webUrl),
  created_by: str,
  created_at: str,
  expires_at: str,
  paid_at: nullable(str),
  paid_by: nullable(str),
});
export type Invoice = Infer<typeof invoiceParser>;

export const invoicesParser = object({ invoices: arrayOf(invoiceParser) });

/** The body of `POST /api/v1/invoices`; `amount` is a string on purpose. */
export interface NewInvoice {
  amount: string;
  memo?: string;
  expires_in_days: number;
}
