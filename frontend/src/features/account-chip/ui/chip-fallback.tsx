"use client";

import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger, cn } from "@evinvest/uikit";
import Link from "next/link";
import { Fragment } from "react";

import type { Caller } from "@/entities/session";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";

import { fallbackIdentity, panelGroups } from "../model/attributes";

/**
 * The chip's stand-in while the platform bundle loads, and for good when it cannot (not
 * deployed, older, unreachable). The avatar alone, the name inside
 * the menu, opening the same entries the chip does, so the caller's own screens and the way out
 * of the session stay on the page whatever happens to the bundle.
 */
export function ChipFallback({ me, onSignOut, className }: { me: Caller; onSignOut: () => void; className?: string }) {
  const t = useT();
  const { label, initials } = fallbackIdentity(me);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className={cn("flex size-8.5 shrink-0 items-center justify-center rounded-full bg-primary-ink/15 text-xs font-semibold text-primary-ink outline-none focus-visible:ring-2 focus-visible:ring-ring", className)}
        aria-label={t("nav.account")}
        title={label}
      >
        <span aria-hidden>{initials}</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuLabel className="min-w-0">
          <span className="block truncate text-sm font-semibold text-ink">{label}</span>
          {label !== me.email && <span className="block truncate text-xs font-normal text-ink-soft">{me.email}</span>}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <Link href={ROUTES.account}>{t("nav.account")}</Link>
        </DropdownMenuItem>
        {panelGroups(me, t).map((group) => (
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
