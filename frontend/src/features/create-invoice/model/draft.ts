import { type NewInvoice, canonicalAmount, expiresInDays, memoProblem } from "@/entities/invoice";
import type { ApiFailure } from "@/shared/api";

export interface InvoiceDraft {
  amount: string;
  memo: string;
  days: string;
}

export type DraftProblem = "amount" | "memo_too_long" | "memo_control" | "days";

export type DraftOutcome = { ok: true; invoice: NewInvoice } | { ok: false; problems: DraftProblem[] };

/** The form's text → the body `POST /api/v1/invoices` takes, or what the backend would refuse. */
export function invoiceFrom(draft: InvoiceDraft): DraftOutcome {
  const amount = canonicalAmount(draft.amount);
  const memo = draft.memo.trim();
  const memoIssue = memoProblem(memo);
  const days = expiresInDays(draft.days);
  const problems: DraftProblem[] = [];
  if (amount === null) problems.push("amount");
  if (memoIssue === "too_long") problems.push("memo_too_long");
  if (memoIssue === "control") problems.push("memo_control");
  if (days === null) problems.push("days");
  if (amount === null || days === null || problems.length > 0) return { ok: false, problems };
  return { ok: true, invoice: { amount, expires_in_days: days, ...(memo === "" ? {} : { memo }) } };
}

/**
 * Whether a failed create may be sent again under the same Idempotency-Key. A lost answer, an
 * outage or a 5xx may have reached banking, so the retry must replay it. A refusal (400, 403,
 * 409) did not create anything, and the same key would only repeat the refusal.
 */
export function keepsKey(failure: ApiFailure): boolean {
  switch (failure.kind) {
    case "network":
    case "unavailable":
    case "failed":
      return true;
    default:
      return false;
  }
}

/** Banking's machine reasons on a 409 (`panel_server::invoices`), as message keys. */
export const CONFLICT_REASONS = ["invoice_not_open", "invoice_processing", "scope_frozen", "idempotency_conflict"] as const;
export type ConflictReason = (typeof CONFLICT_REASONS)[number];

export function conflictReason(failure: ApiFailure): ConflictReason | null {
  if (failure.kind !== "conflict") return null;
  return CONFLICT_REASONS.find((reason) => reason === failure.message) ?? null;
}
