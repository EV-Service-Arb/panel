import { IDEMPOTENCY_HEADER, http } from "@/shared/api";

import { type Invoice, type NewInvoice, invoiceParser, invoicesParser } from "../model/invoice";

/** Newest first, from the panel's storage: no call to banking. */
export async function fetchInvoices(): Promise<Invoice[]> {
  return (await http.get("/api/v1/invoices", invoicesParser)).invoices;
}

/** The row as banking has it now: the backend asks banking and stores the answer. */
export function fetchInvoice(id: string): Promise<Invoice> {
  return http.get(`/api/v1/invoices/${encodeURIComponent(id)}`, invoiceParser);
}

/**
 * Issues an invoice. `key` is required: a lost answer retried without the same key would bill
 * twice. The same key with the same body answers the same row (200), with another body 409.
 */
export function createInvoice(invoice: NewInvoice, key: string): Promise<Invoice> {
  return http.send("POST", "/api/v1/invoices", invoice, invoiceParser, { [IDEMPOTENCY_HEADER]: key });
}
