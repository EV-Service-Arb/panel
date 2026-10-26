"use client";

import { Button, Settled, Skeleton, toast } from "@evinvest/uikit";
import { RefreshCw } from "lucide-react";

import { fetchInvoice, fetchInvoices } from "@/entities/invoice";
import { may, useMe } from "@/entities/session";
import { CreateInvoiceForm } from "@/features/create-invoice";
import { type PayOutcome, payNotice, refetchesAfter } from "@/features/pay-invoice-element";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";
import { notifyFailure } from "@/shared/ui/notify";
import { ScreenFrame } from "@/shared/ui/screen-frame";
import { useButtonSize } from "@/shared/ui/touch";

import { InvoiceTable } from "./invoice-table";

export function InvoicesView() {
  const t = useT();
  const me = useMe();
  const button = useButtonSize();
  const invoices = useResource("invoices", fetchInvoices, "invoices", { live: ["invoices"] });

  if (!may(me, "sa:work:read")) {
    return (
      <ScreenFrame title={t("invoices.title")}>
        <ErrorState failure={{ kind: "forbidden", message: "" }} />
      </ScreenFrame>
    );
  }

  // The popup's word is a hint: the row is read again from banking, and only the row says paid.
  const onPaySettled = (id: string, outcome: PayOutcome) => {
    const notice = payNotice(outcome);
    if (notice === "invoices.pay.sent") toast.info(t(notice));
    else if (notice) toast.error(t(notice));
    if (!refetchesAfter(outcome)) return;
    fetchInvoice(id).then(() => invoices.reload(), (e: unknown) => {
      notifyFailure(e, t);
      invoices.reload();
    });
  };

  const refresh = (
    <Button variant="outline" size={button()} onClick={invoices.reload}>
      <RefreshCw aria-hidden className="size-4" />
      {t("invoices.refresh")}
    </Button>
  );

  return (
    <ScreenFrame title={t("invoices.title")} description={t("invoices.description")} actions={refresh}>
      {may(me, "sa:work:invoices:create") && <CreateInvoiceForm onCreated={invoices.reload} />}
      <Settled loading={invoices.status === "loading"} skeleton={<Skeleton className="h-40 w-full" />}>
        {invoices.status === "error" && <ErrorState failure={invoices.failure} onRetry={invoices.reload} />}
        {invoices.status === "ok" && invoices.data.length === 0 && <EmptyState title={t("invoices.empty")} description={t("invoices.empty.body")} />}
        {invoices.status === "ok" && invoices.data.length > 0 && <InvoiceTable invoices={invoices.data} onPaySettled={onPaySettled} />}
      </Settled>
    </ScreenFrame>
  );
}
