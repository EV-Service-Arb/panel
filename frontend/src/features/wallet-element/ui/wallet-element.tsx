"use client";

import { Skeleton } from "@evinvest/uikit";
import { useEffect, useRef } from "react";

import { useLocale, useT } from "@/shared/i18n";
import { useElementBundle } from "@/shared/mfe";
import { EmptyState } from "@/shared/ui/empty-state";

import { WALLET_API_VERSION, WALLET_SCOPE, WALLET_SCRIPT, WALLET_SCRIPT_INTEGRITY, WALLET_TAG } from "../model/constants";

// A stable object: the loader's effect is keyed on its fields.
const BUNDLE = { minApiVersion: WALLET_API_VERSION, integrity: WALLET_SCRIPT_INTEGRITY, timeoutMs: 8000 } as const;

/**
 * banking's `<mfe-banking-wallet scope="sa">`: balances and recent history of the SA
 * compartment. The element moves no money itself; an action it offers that nobody here
 * intercepts navigates to the wallet's own page (see the PR notes on `ev:wallet-action`).
 *
 * Imperative mount, as the account chip's: the attribute must be set before the element
 * connects. `locale` remounts it, because the element reads `<html lang>` once when it renders.
 */
export function WalletElement() {
  const t = useT();
  const locale = useLocale();
  const bundle = useElementBundle(WALLET_TAG, WALLET_SCRIPT, BUNDLE);
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (bundle !== "ready" || !host) return;
    const element = document.createElement(WALLET_TAG);
    element.setAttribute("scope", WALLET_SCOPE);
    host.appendChild(element);
    return () => element.remove();
  }, [bundle, locale]);

  if (bundle === "failed") return <EmptyState title={t("wallet.unavailable.title")} description={t("wallet.unavailable.body")} />;
  return (
    <div ref={hostRef} aria-busy={bundle === "loading"}>
      {bundle === "loading" && <Skeleton className="h-40 w-full" />}
    </div>
  );
}
