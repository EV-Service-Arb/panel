"use client";

import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  NavigationMenu,
  NavigationMenuItem,
  NavigationMenuLink,
  NavigationMenuList,
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
  TopBar,
  cn,
  navigationMenuTriggerStyle,
} from "@evinvest/uikit";
import { CONCIERGE_ORIGIN } from "@evinvest/concierge";
import { Check, ChevronDown, Menu } from "lucide-react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useState } from "react";

import { DevSignInBadge, ownScreens, seesSites, useMe } from "@/entities/session";
import { useTokens } from "@/entities/tokens";
import { AccountChip } from "@/features/account-chip";
import { useSignOut } from "@/features/sign-out";
import { safeLocalPath } from "@/shared/api";
import { ROUTES } from "@/shared/config/routes";
import { WALLET_PAGE_ENABLED } from "@/shared/config/wallet-page";
import { useT } from "@/shared/i18n";

import { Outline } from "./outline";

/**
 * `[Service-Arb⌄] [Sites] [Review Archive] … [Sources] [Usage tokens] [Wallet] [chip]`; on a phone all but the chip go into
 * a Sheet, with the outline. The end slot is right-anchored and the chip is last, so the tokens appearing or changing width
 * grow leftwards and never move the chip.
 */
export function PanelTopBar() {
  const t = useT();
  const me = useMe();
  const signOut = useSignOut();
  const router = useRouter();
  const pathname = usePathname();
  const apps = [
    ...(seesSites(me) ? [{ href: ROUTES.sites, label: t("nav.sites"), here: pathname.startsWith("/sites") }] : []),
    { href: ROUTES.reviewArchive, label: t("nav.reviewArchive"), here: pathname.startsWith("/review_archive") },
  ];
  const [open, setOpen] = useState(false);

  useEffect(() => {
    // a sites element's link it does not route itself; the account chip's own bubbles from the chip
    const onNavigate = (e: Event) => {
      if (e.target !== window) return;
      const href: unknown = (e as CustomEvent<{ href?: unknown }>).detail?.href;
      const path = typeof href === "string" ? safeLocalPath(href) : null;
      if (path === null) throw new Error(`ev:navigate: not a path on this origin: ${String(href)}`);
      e.preventDefault();
      setOpen(false);
      router.push(path);
    };
    window.addEventListener("ev:navigate", onNavigate);
    return () => window.removeEventListener("ev:navigate", onNavigate);
  }, [router]);

  return (
    <TopBar
      className="px-4 md:px-8"
      start={
        <>
          <Sheet open={open} onOpenChange={setOpen}>
            <SheetTrigger asChild>
              <Button type="button" variant="ghost" size="sm" icon className="md:hidden" aria-label={t("nav.menu")}>
                <Menu aria-hidden />
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="w-72 gap-2 p-4">
              <SheetHeader className="flex-row items-center gap-2 p-0">
                <SheetTitle>{t("app.name")}</SheetTitle>
                <DevSignInBadge />
              </SheetHeader>
              {apps.map((a) => (
                <Link key={a.href} href={a.href} onClick={() => setOpen(false)} aria-current={a.here ? "page" : undefined} className={cn(navigationMenuTriggerStyle(), "w-full justify-start px-2", a.here && "bg-hover/50")}>
                  {a.label}
                </Link>
              ))}
              <div className="border-t border-border pt-2">
                <Outline onNavigate={() => setOpen(false)} />
              </div>
              <OwnScreens className="flex-col items-stretch border-t border-border pt-2" onNavigate={() => setOpen(false)} />
            </SheetContent>
          </Sheet>
          <AppSwitcher />
          <DevSignInBadge className="hidden sm:inline-flex" />
          <NavigationMenu viewport={false} className="ml-4 hidden md:flex" aria-label={t("nav.primary")}>
            <NavigationMenuList>
              {apps.map((a) => (
                <NavigationMenuItem key={a.href}>
                  <NavigationMenuLink asChild className={cn(navigationMenuTriggerStyle(), a.here && "bg-hover/50")}>
                    <Link href={a.href} aria-current={a.here ? "page" : undefined}>
                      {a.label}
                    </Link>
                  </NavigationMenuLink>
                </NavigationMenuItem>
              ))}
            </NavigationMenuList>
          </NavigationMenu>
        </>
      }
      end={
        <>
          <OwnScreens className="hidden md:flex" />
          <AccountChip me={me} onSignOut={signOut} />
        </>
      }
    />
  );
}

/** Service-Arb and evinvest.ltd. */
function AppSwitcher() {
  const t = useT();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger className="flex shrink-0 items-center gap-1 rounded-md px-1 outline-none focus-visible:ring-2 focus-visible:ring-ring" aria-label={t("apps.switch")}>
        <span className="text-base font-semibold whitespace-nowrap text-ink">{t("app.name")}</span>
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

/** The caller's own screens; Tokens is labelled with the count, never called a balance: evinvest.ltd's wallet is another ledger. */
function OwnScreens({ className, onNavigate }: { className?: string; onNavigate?: () => void }) {
  const t = useT();
  const me = useMe();
  const tokens = useTokens();
  const pathname = usePathname();
  const label = {
    sources: t("nav.sources"),
    usage: t("usage.title"),
    tokens: tokens.status === "ok" ? t("tokens.count", { n: tokens.data.balance }) : t("nav.tokens"),
    wallet: t("nav.wallet"),
  };
  return (
    <div className={cn("flex items-center gap-1", className)}>
      {ownScreens(me, WALLET_PAGE_ENABLED).map((id) => {
        const here = pathname.startsWith(ROUTES[id]);
        return (
          <Link key={id} href={ROUTES[id]} onClick={() => onNavigate?.()} aria-current={here ? "page" : undefined} className={cn(navigationMenuTriggerStyle(), "justify-start", here && "bg-hover/50")}>
            <span className={cn(id === "tokens" && "tabular-nums")}>{label[id]}</span>
          </Link>
        );
      })}
    </div>
  );
}
