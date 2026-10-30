"use client";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  TopBar,
  cn,
} from "@evinvest/uikit";
import { CONCIERGE_ORIGIN } from "@evinvest/concierge";
import { Check, ChevronDown } from "lucide-react";
import Link from "next/link";

import { useMe } from "@/entities/session";
import { useTokens } from "@/entities/tokens";
import { AccountChip } from "@/features/account-chip";
import { useSignOut } from "@/features/sign-out";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { LiveStatusIndicator } from "@/shared/ui/live-status";

/**
 * The wide screen's top bar (the kit's contract): where you are, your tokens, the connection, who
 * you are. The end slot is right-anchored and the chip is last, so the tokens and the connection
 * appearing or changing width grow leftwards and never move the chip.
 */
export function PanelTopBar() {
  const me = useMe();
  const signOut = useSignOut();
  return (
    <TopBar
      start={<AppSwitcher />}
      end={
        <>
          <TokensStat />
          <LiveStatusIndicator />
          <AccountChip me={me} onSignOut={signOut} />
        </>
      }
    />
  );
}

/** Service-Arb and evinvest.ltd. */
function AppSwitcher() {
  const t = useT();
  const name = <span className="text-base font-semibold text-ink">{t("app.name")}</span>;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger className="flex items-center gap-1 rounded-md px-1 outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label={t("apps.switch")}>
        {name}
        <ChevronDown aria-hidden className="size-4 text-ink-soft" />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="w-56">
        <DropdownMenuItem asChild>
          <Link href="/" aria-current="true">
            <Check aria-hidden className="size-4" />
            {t("app.name")}
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem asChild>
          <a href={CONCIERGE_ORIGIN}>
            <span aria-hidden className="size-4" />
            evinvest.ltd
          </a>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Review-archive tokens, never called a balance: evinvest.ltd's wallet is another ledger. Absent when unknown. */
function TokensStat() {
  const t = useT();
  const tokens = useTokens();
  if (tokens === null) return null;
  return (
    <a
      href={ROUTES.reviewArchiveTokens}
      className={cn("text-sm tabular-nums hover:underline", tokens.balance > 0 ? "text-ink" : "text-ink-soft")}
      title={t("account.tokens.renewal", { daily: tokens.daily, cap: tokens.cap })}
    >
      {t("tokens.count", { n: tokens.balance })}
    </a>
  );
}
