"use client";

import { Card, CardContent, Table, TableBody, TableCell, TableHead, TableHeader, TableRow, cn } from "@evinvest/uikit";

import { type Invoice, InvoiceStateBadge, payableInvoiceId } from "@/entities/invoice";
import { type PayOutcome, PayInvoiceElement } from "@/features/pay-invoice-element";
import { useLocale, useT } from "@/shared/i18n";
import { formatDateTime } from "@/shared/lib/format";
import { DESKTOP_QUERY, useMediaQuery } from "@/shared/lib/use-media-query";
import { EDGE_CELL, TABLE_HEAD } from "@/shared/ui/table";

const HEAD = cn(TABLE_HEAD, EDGE_CELL);

type OnPaySettled = (id: string, outcome: PayOutcome) => void;

/** The invoices in one card: a table from `md` up, stacked rows on a phone, as Sources does. */
export function InvoiceTable({ invoices, onPaySettled }: { invoices: Invoice[]; onPaySettled: OnPaySettled }) {
  const isDesktop = useMediaQuery(DESKTOP_QUERY);
  return (
    <Card className="overflow-hidden py-0">
      <CardContent className="p-0">{isDesktop ? <Wide invoices={invoices} onPaySettled={onPaySettled} /> : <Narrow invoices={invoices} onPaySettled={onPaySettled} />}</CardContent>
    </Card>
  );
}

/** The amount as the backend wrote it: a canonical decimal, never rounded through a float. */
function amountOf(invoice: Invoice): string {
  return `${invoice.amount} ${invoice.currency}`;
}

/** The state from the row, with when it was paid; the pay button only while the row is open. */
function StateCell({ invoice, onPaySettled }: { invoice: Invoice; onPaySettled: OnPaySettled }) {
  const t = useT();
  const locale = useLocale();
  const payable = payableInvoiceId(invoice);
  return (
    <div className="flex flex-col items-start gap-2">
      <InvoiceStateBadge state={invoice.state} />
      {invoice.state === "paid" && invoice.paid_at && <span className="text-xs tabular-nums text-ink-soft">{t("invoices.paidAt", { at: formatDateTime(invoice.paid_at, locale) })}</span>}
      {invoice.state === "processing" && <span className="text-xs text-ink-soft">{t("invoices.processing.hint")}</span>}
      {payable && <PayInvoiceElement invoiceId={payable} onSettled={(outcome) => onPaySettled(invoice.id, outcome)} />}
    </div>
  );
}

function Wide({ invoices, onPaySettled }: { invoices: Invoice[]; onPaySettled: OnPaySettled }) {
  const t = useT();
  const locale = useLocale();
  return (
    <Table>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <TableHead className={HEAD}>{t("invoices.created.at")}</TableHead>
          <TableHead className={cn(HEAD, "text-right")}>{t("invoices.amount")}</TableHead>
          <TableHead className={HEAD}>{t("invoices.memo")}</TableHead>
          <TableHead className={HEAD}>{t("invoices.expires")}</TableHead>
          <TableHead className={HEAD}>{t("invoices.state")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {invoices.map((invoice) => (
          <TableRow key={invoice.id}>
            <TableCell className={cn(EDGE_CELL, "align-top tabular-nums text-ink-soft")}>{formatDateTime(invoice.created_at, locale)}</TableCell>
            <TableCell className={cn(EDGE_CELL, "align-top text-right tabular-nums")}>{amountOf(invoice)}</TableCell>
            <TableCell className={cn(EDGE_CELL, "max-w-80 align-top break-words whitespace-normal text-ink-mid")}>{invoice.memo}</TableCell>
            <TableCell className={cn(EDGE_CELL, "align-top tabular-nums text-ink-soft")}>{formatDateTime(invoice.expires_at, locale)}</TableCell>
            <TableCell className={cn(EDGE_CELL, "align-top")}>
              <StateCell invoice={invoice} onPaySettled={onPaySettled} />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function Narrow({ invoices, onPaySettled }: { invoices: Invoice[]; onPaySettled: OnPaySettled }) {
  const t = useT();
  const locale = useLocale();
  return (
    <ul className="divide-y divide-border">
      {invoices.map((invoice) => (
        <li key={invoice.id} className="flex items-start justify-between gap-3 px-4 py-3">
          <div className="flex min-w-0 flex-col gap-0.5">
            <span className="font-medium tabular-nums">{amountOf(invoice)}</span>
            {invoice.memo && <span className="break-words text-sm text-ink-mid">{invoice.memo}</span>}
            <span className="text-xs tabular-nums text-ink-soft">{formatDateTime(invoice.created_at, locale)}</span>
            <span className="text-xs tabular-nums text-ink-soft">{t("invoices.expiresAt", { at: formatDateTime(invoice.expires_at, locale) })}</span>
          </div>
          <div className="shrink-0">
            <StateCell invoice={invoice} onPaySettled={onPaySettled} />
          </div>
        </li>
      ))}
    </ul>
  );
}
