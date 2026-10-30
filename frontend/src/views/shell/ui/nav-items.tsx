import { NavDot, type NavGroup, type NavItem } from "@evinvest/uikit";
import { Archive, ArrowUpRight, BarChart3, Ellipsis, FlaskConical, Gauge, Inbox, KeyRound, LayoutGrid, MapPin, ReceiptText, Tags, UserRound, Wallet } from "lucide-react";

import { type Caller, may } from "@/entities/session";
import { ROUTES } from "@/shared/config/routes";
import { WALLET_PAGE_ENABLED } from "@/shared/config/wallet-page";
import type { T } from "@/shared/i18n";

import type { Marks } from "../model/use-marks";

export interface PanelNav {
  groups: NavGroup[];
  /** Pinned to the rail's bottom: what is the caller's own rather than a section's. */
  footer: NavGroup[];
  tabs: NavItem[];
}

/**
 * The rail (spec §10, hybrid 1+2) in groups, one per section the caller's
 * permissions open, and the phone's tabs: the three daily screens, and More
 * standing in for the rest (Pricing among them: daily for nobody). Marks ride on
 * both: a count where there is one, a dot where "something changed" is all there is.
 */
/** The screens behind a build flag, and whether this build offers each. */
export interface FlaggedPages {
  wallet: boolean;
}

const BUILD_PAGES: FlaggedPages = { wallet: WALLET_PAGE_ENABLED };

/** `pages`: the build flags by default; a parameter so that a test can turn them on and off. */
export function panelNav(t: T, caller: Caller, marks: Marks, pages: FlaggedPages = BUILD_PAGES): PanelNav {
  const work = may(caller, "sa:work:read");
  const analysis = may(caller, "sa:analysis:read");
  const admin = may(caller, "sa:admin:sources:manage");
  const dot = (corner: boolean) => <NavDot corner={corner} label={t("nav.changed")} />;
  const overview: NavItem = { id: "overview", href: ROUTES.overview, label: t("nav.overview"), icon: BarChart3 };
  // New leads are counted; bookings without one only say "something waits", and a count says it already.
  const leadsBadge = marks.leads > 0 || !marks.bookings ? marks.leads : <NavDot corner={false} label={t("nav.unmatched")} />;
  const leads: NavItem = { id: "leads", href: ROUTES.leads, label: t("nav.leads"), icon: Inbox, badge: leadsBadge };
  const places: NavItem = { id: "places", href: ROUTES.places, label: t("nav.places"), icon: MapPin, badge: marks.places };
  const pricing: NavItem = { id: "pricing", href: ROUTES.pricing, label: t("nav.pricing"), icon: Tags };
  const experiments: NavItem = { id: "experiments", href: ROUTES.experiments, label: t("nav.experiments"), icon: FlaskConical, ...(marks.experiments ? { badge: dot(false) } : {}) };
  const sources: NavItem = { id: "sources", href: ROUTES.sources, label: t("nav.sources"), icon: KeyRound, ...(marks.sources ? { badge: dot(false) } : {}) };
  const grafana: NavItem = {
    id: "grafana",
    href: ROUTES.grafana,
    label: t("nav.grafana"),
    icon: LayoutGrid,
    external: true,
    target: "_blank",
    trailing: <ArrowUpRight aria-hidden className="size-4 text-ink-soft" />,
  };
  const reviewArchive: NavItem = { id: "review_archive", href: ROUTES.reviewArchive, label: t("nav.reviewArchive"), icon: Archive };
  const walletItem: NavItem = { id: "wallet", href: ROUTES.wallet, label: t("nav.wallet"), icon: Wallet };
  const invoicesItem: NavItem = { id: "invoices", href: ROUTES.invoices, label: t("nav.invoices"), icon: ReceiptText };
  const wallet = work && pages.wallet ? [walletItem] : [];
  const account: NavItem = { id: "account", href: ROUTES.account, label: t("nav.account"), icon: UserRound };
  const usage: NavItem = { id: "usage", href: ROUTES.usage, label: t("usage.title"), icon: Gauge };
  const elsewhere = (analysis && marks.experiments) || (admin && marks.sources);
  const more: NavItem = {
    id: "more",
    href: ROUTES.more,
    label: t("nav.more"),
    icon: Ellipsis,
    also: [
      ...(work ? [ROUTES.reviewArchive, ROUTES.pricing, ROUTES.invoices] : []),
      ...wallet.map((item) => item.href),
      ROUTES.account,
      ROUTES.usage,
      ...(analysis ? [ROUTES.experiments] : []),
      ...(admin ? [ROUTES.sources] : []),
    ],
    ...(elsewhere ? { badge: dot(true) } : {}),
  };

  const groups: NavGroup[] = [];
  if (work) groups.push({ id: "work", label: t("nav.group.work"), items: [overview, leads, places, pricing, invoicesItem] });
  if (analysis) groups.push({ id: "analysis", label: t("nav.group.analysis"), items: [experiments, grafana] });
  if (admin) groups.push({ id: "admin", label: t("nav.group.admin"), items: [sources] });
  groups.push({ id: "archive", label: t("nav.group.archive"), items: [reviewArchive] });
  return {
    groups,
    footer: [{ id: "own", items: [...wallet, account, usage] }],
    tabs: [...(work ? [overview, leads, places] : [reviewArchive]), more],
  };
}
