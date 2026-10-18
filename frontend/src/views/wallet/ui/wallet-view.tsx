"use client";

import { WalletElement } from "@/features/wallet-element";
import { WALLET_PAGE_ENABLED } from "@/shared/config/wallet-page";
import { useT } from "@/shared/i18n";
import { EmptyState } from "@/shared/ui/empty-state";
import { ScreenFrame } from "@/shared/ui/screen-frame";

/** The wallet screen; with the flag off the route exists in the export but shows no wallet. */
export function WalletView() {
  const t = useT();
  return (
    <ScreenFrame title={t("wallet.title")} {...(WALLET_PAGE_ENABLED ? { description: t("wallet.description") } : {})} width="content">
      {WALLET_PAGE_ENABLED ? <WalletElement /> : <EmptyState title={t("wallet.unavailable.title")} description={t("wallet.off.body")} />}
    </ScreenFrame>
  );
}
