"use client";

import { Button, Item, ItemActions, ItemContent, ItemGroup, ItemTitle, NavDot } from "@evinvest/uikit";
import { Archive, ArrowUpRight, CircleUser, FlaskConical, KeyRound, LayoutGrid, type LucideIcon, ReceiptText, Tags, Users, Wallet } from "lucide-react";
import Link from "next/link";

import { DevSignInBadge, may, useMe } from "@/entities/session";
import { useSignOut } from "@/features/sign-out";
import { switchAccountPath } from "@/shared/api";
import { INVOICES_PAGE_ENABLED } from "@/shared/config/invoices-page";
import { ROUTES } from "@/shared/config/routes";
import { WALLET_PAGE_ENABLED } from "@/shared/config/wallet-page";
import { type MessageKey, useT } from "@/shared/i18n";
import { useNavMark } from "@/shared/lib/nav-marks";
import { LiveStatusIndicator } from "@/shared/ui/live-status";
import { ScreenFrame } from "@/shared/ui/screen-frame";
import { useButtonSize } from "@/shared/ui/touch";

/** The phone's fourth tab: what the rail holds beyond the three screens, and who is signed in. */
export function MoreView() {
  const t = useT();
  const button = useButtonSize();
  const me = useMe();
  const signOut = useSignOut();
  return (
    <ScreenFrame title={t("more.title")}>
      <section className="flex flex-col gap-1" aria-label={t("nav.account")}>
        <span className="flex flex-wrap items-center gap-2 text-sm text-ink">
          {me.preferred_name || me.email} <DevSignInBadge />
        </span>
        <LiveStatusIndicator className="mt-1" />
      </section>
      <ItemGroup className="gap-2">
        <MoreLink href={ROUTES.account} icon={CircleUser} label="account.title" />
        {may(me, "sa:review_archive:members:act_as") && <MoreLink href={ROUTES.reviewArchiveActAs} icon={Users} label="account.actAs" />}
        {may(me, "sa:work:read") && <MoreLink href={ROUTES.reviewArchive} icon={Archive} label="nav.reviewArchive" />}
        {may(me, "sa:work:read") && <MoreLink href={ROUTES.pricing} icon={Tags} label="nav.pricing" />}
        {WALLET_PAGE_ENABLED && may(me, "sa:work:read") && <MoreLink href={ROUTES.wallet} icon={Wallet} label="nav.wallet" />}
        {INVOICES_PAGE_ENABLED && may(me, "sa:work:read") && <MoreLink href={ROUTES.invoices} icon={ReceiptText} label="nav.invoices" />}
        {may(me, "sa:analysis:read") && <MoreLink href={ROUTES.experiments} icon={FlaskConical} label="nav.experiments" />}
        {may(me, "sa:admin:sources:manage") && <MoreLink href={ROUTES.sources} icon={KeyRound} label="nav.sources" />}
        <Item variant="outline" size="sm" asChild>
          <a href={ROUTES.grafana} target="_blank" rel="noopener noreferrer">
            <LayoutGrid aria-hidden className="size-4" />
            <ItemContent>
              <ItemTitle>{t("nav.grafana")}</ItemTitle>
            </ItemContent>
            <ArrowUpRight aria-hidden className="size-4" />
          </a>
        </Item>
      </ItemGroup>
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" size={button()} asChild>
          <a href={switchAccountPath(ROUTES.more)}>{t("nav.switchAccount")}</a>
        </Button>
        <Button variant="outline" size={button()} onClick={signOut}>
          {t("nav.signOut")}
        </Button>
      </div>
    </ScreenFrame>
  );
}

function MoreLink({ href, icon: Icon, label }: { href: string; icon: LucideIcon; label: MessageKey }) {
  const t = useT();
  const mark = useNavMark(href);
  const changed = mark === true || (typeof mark === "number" && mark > 0);
  return (
    <Item variant="outline" size="sm" asChild>
      <Link href={href}>
        <Icon aria-hidden className="size-4" />
        <ItemContent>
          <ItemTitle>{t(label)}</ItemTitle>
        </ItemContent>
        {changed && (
          <ItemActions>
            <NavDot label={t("nav.changed")} />
          </ItemActions>
        )}
      </Link>
    </Item>
  );
}
