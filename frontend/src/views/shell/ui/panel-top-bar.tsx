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
  NavigationMenuTrigger,
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
  TopBar,
  cn,
  navigationMenuTriggerStyle,
  useDismissableLayer,
} from "@evinvest/uikit";
import { CONCIERGE_ORIGIN } from "@evinvest/concierge";
import { Check, ChevronDown, Menu } from "lucide-react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";

import { DevSignInBadge, seesSites, useMe } from "@/entities/session";
import { useTokens } from "@/entities/tokens";
import { AccountChip } from "@/features/account-chip";
import { useSignOut } from "@/features/sign-out";
import { SitesElement } from "@/features/sites-element";
import { safeLocalPath } from "@/shared/api";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";

type Open = "sites" | "sheet" | null;

/**
 * `[Service-Arb⌄] [Sites⌄] [Review Archive] … [tokens] [chip]`; on a phone the two sections go into
 * a Sheet. The Sites items are `mfe-sites-nav`'s: the conductor knows none of the sites' routes.
 * The end slot is right-anchored and the chip is last, so the tokens appearing or changing width
 * grow leftwards and never move the chip.
 */
export function PanelTopBar() {
  const t = useT();
  const me = useMe();
  const signOut = useSignOut();
  const router = useRouter();
  const archive = usePathname().startsWith("/review_archive");
  const sites = seesSites(me);
  const [open, setOpen] = useState<Open>(null);

  useEffect(() => {
    // a sites element's link it does not route itself; the account chip's own bubbles from the chip
    const onNavigate = (e: Event) => {
      if (e.target !== window) return;
      const href: unknown = (e as CustomEvent<{ href?: unknown }>).detail?.href;
      const path = typeof href === "string" ? safeLocalPath(href) : null;
      if (path === null) throw new Error(`ev:navigate: not a path on this origin: ${String(href)}`);
      e.preventDefault();
      setOpen(null);
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
          <Sheet open={open === "sheet"} onOpenChange={(o) => setOpen(o ? "sheet" : null)}>
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
              {sites && (
                // A click on one of its links has routed (CONTRACT §9): the sheet is done.
                <section aria-label={t("nav.sites")} onClick={() => setOpen(null)}>
                  <h2 className="px-2 pt-2 text-xs font-medium uppercase tracking-wide text-ink-soft">{t("nav.sites")}</h2>
                  <SitesElement tag="mfe-sites-nav" />
                </section>
              )}
              <Link href={ROUTES.reviewArchive} onClick={() => setOpen(null)} aria-current={archive ? "page" : undefined} className={cn(navigationMenuTriggerStyle(), "w-full justify-start px-2", archive && "bg-hover/50")}>
                {t("nav.reviewArchive")}
              </Link>
              <TokensStat className="px-2 pt-2" />
            </SheetContent>
          </Sheet>
          <AppSwitcher />
          <DevSignInBadge className="hidden sm:inline-flex" />
          <NavigationMenu viewport={false} className="ml-4 hidden md:flex" aria-label={t("nav.primary")}>
            <NavigationMenuList>
              {sites && <SitesMenu open={open === "sites"} onOpenChange={(o) => setOpen(o ? "sites" : null)} />}
              <NavigationMenuItem>
                <NavigationMenuLink asChild className={cn(navigationMenuTriggerStyle(), archive && "bg-hover/50")}>
                  <Link href={ROUTES.reviewArchive} aria-current={archive ? "page" : undefined}>
                    {t("nav.reviewArchive")}
                  </Link>
                </NavigationMenuLink>
              </NavigationMenuItem>
            </NavigationMenuList>
          </NavigationMenu>
        </>
      }
      end={
        <>
          <TokensStat className="hidden sm:block" />
          <AccountChip me={me} onSignOut={signOut} />
        </>
      }
    />
  );
}

/**
 * Mounted while closed, only hidden (CONTRACT §9): the element keeps its badges and the tab
 * title's count on every screen, and opens with them already drawn. On a phone the whole menu is
 * hidden by CSS and this stays the always-mounted instance.
 */
function SitesMenu({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const t = useT();
  const here = usePathname().startsWith("/sites");
  const itemRef = useRef<HTMLLIElement>(null);
  const panelRef = useDismissableLayer({ enabled: open, onDismiss: () => onOpenChange(false), exclude: [itemRef] });
  return (
    <NavigationMenuItem ref={itemRef} open={open} onOpenChange={onOpenChange}>
      <NavigationMenuTrigger className={cn(here && "bg-hover/50")}>{t("nav.sites")}</NavigationMenuTrigger>
      <div
        ref={panelRef}
        hidden={!open}
        onClick={() => onOpenChange(false)}
        className="absolute top-full left-0 z-50 mt-1.5 min-w-56 rounded-md border border-border bg-popover p-2 text-ink shadow"
      >
        <SitesElement tag="mfe-sites-nav" />
      </div>
    </NavigationMenuItem>
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

/** Review-archive tokens, never called a balance: evinvest.ltd's wallet is another ledger. Absent when unknown. */
function TokensStat({ className }: { className?: string }) {
  const t = useT();
  const tokens = useTokens();
  if (tokens === null) return null;
  return (
    <a
      href={ROUTES.reviewArchiveTokens}
      className={cn("text-sm tabular-nums hover:underline", tokens.balance > 0 ? "text-ink" : "text-ink-soft", className)}
      title={t("account.tokens.renewal", { daily: tokens.daily, cap: tokens.cap })}
    >
      {t("tokens.count", { n: tokens.balance })}
    </a>
  );
}
