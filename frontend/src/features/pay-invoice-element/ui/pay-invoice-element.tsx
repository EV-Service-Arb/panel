"use client";

import { Skeleton } from "@evinvest/uikit";
import { useEffect, useRef } from "react";

import { WALLET_API_VERSION, WALLET_SCOPE, WALLET_SCRIPT, WALLET_SCRIPT_INTEGRITY } from "@/shared/config/wallet-bundle";
import { useLocale, useT } from "@/shared/i18n";
import { useElementBundle } from "@/shared/mfe";

import { type PayOutcome, WALLET_ACTION_EVENT, WALLET_RESULT_MESSAGE, payActionFromEvent, payResultFromEvent } from "../model/action";
import { openPayPopup, payNavigationHref } from "../model/popup";

export const PAY_INVOICE_TAG = "mfe-banking-pay-invoice";

// A stable object: the loader's effect is keyed on its fields.
const BUNDLE = { minApiVersion: WALLET_API_VERSION, integrity: WALLET_SCRIPT_INTEGRITY, timeoutMs: 8000 } as const;

/**
 * banking's `<mfe-banking-pay-invoice invoice scope="sa" actions="inline">`. The element pays in
 * this page when banking lists `pay` for it, and reports how its form closed (`ev:wallet-result`).
 * Otherwise it offers the payment as a cancelable `ev:wallet-action`; this host opens it in a
 * popup on evinvest.ltd and reports how it ended. `onSettled` gets no "paid": the caller reads the row.
 *
 * Imperative mount, as the wallet's: the attributes must be set before the element connects.
 * `locale` remounts it, because the element reads `<html lang>` once.
 */
export function PayInvoiceElement({ invoiceId, onSettled }: { invoiceId: string; onSettled: (outcome: PayOutcome) => void }) {
  const t = useT();
  const locale = useLocale();
  const bundle = useElementBundle(PAY_INVOICE_TAG, WALLET_SCRIPT, BUNDLE);
  const hostRef = useRef<HTMLDivElement>(null);
  const settledRef = useRef(onSettled);

  useEffect(() => {
    settledRef.current = onSettled;
  });

  useEffect(() => {
    const host = hostRef.current;
    if (bundle !== "ready" || !host) return;
    const element = document.createElement(PAY_INVOICE_TAG);
    element.setAttribute("invoice", invoiceId);
    element.setAttribute("scope", WALLET_SCOPE);
    element.setAttribute("actions", "inline");
    // Synchronous, inside the click: a popup opened after an await is blocked. Every action is
    // cancelled here, a refused one too: left to the element, it would navigate this tab to it.
    const onAction = (event: Event) => {
      event.preventDefault();
      const url = payActionFromEvent(event);
      if (!url) {
        settledRef.current({ status: "invalid_url" });
        return;
      }
      void openPayPopup(url).then((outcome) => {
        // The pay page posts back only to a popup. A full navigation reaches the payment anyway.
        if (outcome.status === "blocked") window.location.assign(payNavigationHref(url));
        else if (outcome.status !== "busy") settledRef.current(outcome);
      });
    };
    const onResult = (event: Event) => {
      const result = payResultFromEvent(event);
      if (result) settledRef.current({ status: "result", result });
    };
    element.addEventListener(WALLET_ACTION_EVENT, onAction);
    element.addEventListener(WALLET_RESULT_MESSAGE, onResult);
    host.appendChild(element);
    return () => {
      element.removeEventListener(WALLET_ACTION_EVENT, onAction);
      element.removeEventListener(WALLET_RESULT_MESSAGE, onResult);
      element.remove();
    };
  }, [bundle, invoiceId, locale]);

  if (bundle === "failed") return <p className="text-sm text-ink-soft">{t("invoices.pay.unavailable")}</p>;
  return (
    <div ref={hostRef} aria-busy={bundle === "loading"}>
      {bundle === "loading" && <Skeleton className="h-9 w-28" />}
    </div>
  );
}
