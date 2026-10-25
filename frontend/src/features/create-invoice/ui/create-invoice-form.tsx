"use client";

import { Button, Field, FieldDescription, FieldError, FieldLabel, Input, toast } from "@evinvest/uikit";
import { useId, useState } from "react";

import { type Invoice, MAX_EXPIRES_IN_DAYS, MAX_MEMO_CHARS, MIN_EXPIRES_IN_DAYS, createInvoice } from "@/entities/invoice";
import { ApiError } from "@/shared/api";
import { type T, useT } from "@/shared/i18n";
import { useKeyedWrite } from "@/shared/lib/use-keyed-write";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize, useControlSize } from "@/shared/ui/touch";

import { type DraftProblem, type InvoiceDraft, conflictReason, invoiceFrom, keepsKey } from "../model/draft";

const EMPTY: InvoiceDraft = { amount: "", memo: "", days: "7" };

/** A new USDT invoice: the amount, a memo the payer reads, and how many days it stays open. */
export function CreateInvoiceForm({ onCreated }: { onCreated: (invoice: Invoice) => void }) {
  const t = useT();
  const button = useButtonSize();
  const size = useControlSize();
  const id = useId();
  const [draft, setDraft] = useState<InvoiceDraft>(EMPTY);
  const write = useKeyedWrite();
  const outcome = invoiceFrom(draft);
  // The amount starts empty; it is wrong only once something is typed.
  const problems = outcome.ok ? [] : outcome.problems.filter((p) => p !== "amount" || draft.amount.trim() !== "");
  const has = (p: DraftProblem) => problems.includes(p);
  const set = (field: keyof InvoiceDraft) => (e: { target: { value: string } }) => setDraft((d) => ({ ...d, [field]: e.target.value }));

  const submit = async () => {
    if (!outcome.ok) return;
    // A box, not a `let`: the compiler does not see the assignment inside the callback.
    const created: { invoice: Invoice | null } = { invoice: null };
    const result = await write.run(outcome.invoice, async (key) => {
      created.invoice = await createInvoice(outcome.invoice, key);
    });
    if (!result.ok) {
      failed(result.error, t, write.forget);
      return;
    }
    toast.positive(t("invoices.created"));
    setDraft(EMPTY);
    if (created.invoice) onCreated(created.invoice);
  };

  return (
    <form
      className="grid gap-3 md:grid-cols-(--grid-invoice-form) md:items-start"
      aria-label={t("invoices.create.title")}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-amount`}>{t("invoices.amount")}</FieldLabel>
        <Input id={`${id}-amount`} size={size} inputMode="decimal" autoComplete="off" aria-invalid={has("amount")} aria-describedby={`${id}-amount-hint`} value={draft.amount} onChange={set("amount")} />
        <FieldDescription id={`${id}-amount-hint`}>{has("amount") ? t("invoices.amount.invalid") : t("invoices.amount.hint")}</FieldDescription>
      </Field>
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-memo`}>{t("invoices.memo")}</FieldLabel>
        <Input id={`${id}-memo`} size={size} autoComplete="off" aria-invalid={has("memo_too_long") || has("memo_control")} aria-describedby={`${id}-memo-hint`} value={draft.memo} onChange={set("memo")} />
        <FieldDescription id={`${id}-memo-hint`}>{t("invoices.memo.hint", { n: MAX_MEMO_CHARS })}</FieldDescription>
      </Field>
      <Field className="flex flex-col gap-1">
        <FieldLabel htmlFor={`${id}-days`}>{t("invoices.days")}</FieldLabel>
        <Input id={`${id}-days`} size={size} type="number" inputMode="numeric" min={MIN_EXPIRES_IN_DAYS} max={MAX_EXPIRES_IN_DAYS} step={1} aria-invalid={has("days")} aria-describedby={`${id}-days-hint`} value={draft.days} onChange={set("days")} />
        <FieldDescription id={`${id}-days-hint`}>{t("invoices.days.hint", { min: MIN_EXPIRES_IN_DAYS, max: MAX_EXPIRES_IN_DAYS })}</FieldDescription>
      </Field>
      <Button type="submit" size={button()} className="md:mt-6" disabled={write.busy || !outcome.ok}>
        {t("invoices.create")}
      </Button>
      {(has("memo_too_long") || has("memo_control")) && <FieldError className="md:col-span-full">{t(has("memo_too_long") ? "invoices.memo.tooLong" : "invoices.memo.control", { n: MAX_MEMO_CHARS })}</FieldError>}
    </form>
  );
}

/** A refusal drops the key, so the next submit is a new request; anything else keeps it for the retry. */
function failed(error: unknown, t: T, forget: () => void): void {
  if (!(error instanceof ApiError)) {
    notifyFailure(error, t);
    return;
  }
  if (!keepsKey(error.failure)) forget();
  const reason = conflictReason(error.failure);
  if (reason) toast.error(t(`invoices.conflict.${reason}`));
  else if (error.failure.kind === "unavailable") toast.error(t("invoices.unavailable"));
  else notifyFailure(error, t);
}
