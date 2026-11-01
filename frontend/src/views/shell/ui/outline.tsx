"use client";

import { cn } from "@evinvest/uikit";
import Link from "next/link";
import { usePathname } from "next/navigation";

import { seesSites, useMe } from "@/entities/session";
import { SitesElement } from "@/features/sites-element";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";

import { GmbTree } from "./gmb-tree";

const ITEM = "rounded-md px-2 py-1.5 text-sm text-ink-soft hover:bg-hover/50 hover:text-ink";

/**
 * The left of every page: the open app's own views, then the caller's GMBs. `mfe-sites-nav` stays
 * mounted, only hidden, off the sites (CONTRACT §9): it keeps the tab title's count on every page.
 */
export function Outline({ onNavigate }: { onNavigate?: () => void }) {
  const t = useT();
  const me = useMe();
  const pathname = usePathname();
  const app = pathname.startsWith("/sites") ? "sites" : pathname.startsWith("/review_archive") ? "archive" : null;
  return (
    <div className="flex flex-col gap-4">
      {seesSites(me) && (
        // A click on one of its links has routed (CONTRACT §9).
        <section hidden={app !== "sites"} aria-label={t("nav.sites")} onClick={() => onNavigate?.()}>
          <SitesElement tag="mfe-sites-nav" />
        </section>
      )}
      {app === "archive" && <ArchiveViews pathname={pathname} {...(onNavigate && { onNavigate })} />}
      <GmbTree {...(onNavigate && { onNavigate })} />
    </div>
  );
}

/** The review archive's views beside its places: full navigations, its element routes them. */
function ArchiveViews({ pathname, onNavigate }: { pathname: string; onNavigate?: () => void }) {
  const t = useT();
  const views = [
    { href: ROUTES.reviewArchive, label: t("reviewArchive.places") },
    { href: ROUTES.reviewArchiveTelegram, label: t("reviewArchive.telegram") },
    { href: ROUTES.reviewArchiveSettings, label: t("reviewArchive.settings") },
  ];
  return (
    <section aria-label={t("nav.reviewArchive")} className="flex flex-col gap-0.5">
      {views.map((v) => {
        const here = pathname.replace(/\/$/, "") === v.href.replace(/\/$/, "");
        return (
          <Link key={v.href} href={v.href} onClick={() => onNavigate?.()} aria-current={here ? "page" : undefined} className={cn(ITEM, here && "bg-hover/50 text-ink")}>
            {v.label}
          </Link>
        );
      })}
    </section>
  );
}
