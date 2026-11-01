"use client";

import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger, cn } from "@evinvest/uikit";
import { ChevronDown } from "lucide-react";
import Link from "next/link";
import { Fragment } from "react";

import type { Caller } from "@/entities/session";
import { ROUTES } from "@/shared/config/routes";
import { WALLET_PAGE_ENABLED } from "@/shared/config/wallet-page";
import { useT } from "@/shared/i18n";

import { fallbackIdentity, panelGroups } from "../model/attributes";

/**
 * The chip's stand-in while the platform bundle loads, and for good when it cannot (not
 * deployed, older, unreachable, `PANEL_CSP_PLATFORM_MFE=0`). Drawn in the chip's box — avatar 34,
 * gap 10, the name, the chevron — and opening the same entries the chip does, so the caller's own
 * screens and the way out of the session stay on the page whatever happens to the bundle.
 */
export function ChipFallback({ me, onSignOut, className }: { me: Caller; onSignOut: () => void; className?: string }) {
  const t = useT();
  const { label, initials } = fallbackIdentity(me);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className={cn("flex min-w-0 items-center gap-2.5 rounded-md py-1 pr-1.5 pl-1.5 outline-none focus-visible:ring-2 focus-visible:ring-ring", className)}
        aria-label={t("nav.account")}
      >
        <span aria-hidden className="flex size-8.5 shrink-0 items-center justify-center rounded-full bg-primary-ink/15 text-xs font-semibold text-primary-ink">
          {initials}
        </span>
        <span className="hidden max-w-36 truncate text-sm font-semibold text-ink sm:inline" title={me.email}>
          {label}
        </span>
        <ChevronDown aria-hidden className="size-4 text-ink-soft" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuItem asChild>
          <Link href={ROUTES.account}>{t("nav.account")}</Link>
        </DropdownMenuItem>
        {panelGroups(me, t, WALLET_PAGE_ENABLED).map((group) => (
          <Fragment key={group.id}>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              {group.items.map((item) => (
                <DropdownMenuItem key={item.id} asChild>
                  {item.external ? <a href={item.href}>{item.label}</a> : <Link href={item.href}>{item.label}</Link>}
                </DropdownMenuItem>
              ))}
            </DropdownMenuGroup>
          </Fragment>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={onSignOut}>{t("nav.signOut")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
