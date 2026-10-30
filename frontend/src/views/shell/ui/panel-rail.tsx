"use client";

import { type NavGroup, ShellNav } from "@evinvest/uikit";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";

import { DevSignInBadge } from "@/entities/session";
import { useT } from "@/shared/i18n";

/** The desktop rail: the app's name (and the dev sign-in warning), then the groups; who is signed in is the top bar's. */
export function PanelRail({ groups, footer }: { groups: NavGroup[]; footer: NavGroup[] }) {
  const t = useT();
  const router = useRouter();
  return (
    <ShellNav
      groups={groups}
      footerGroups={footer}
      pathname={usePathname()}
      linkComponent={Link}
      onItemIntent={(item) => {
        if (!item.external) router.prefetch(item.href);
      }}
      header={
        <div className="flex flex-col items-start gap-2 px-3">
          <span className="text-base font-semibold text-ink">{t("app.name")}</span>
          <DevSignInBadge />
        </div>
      }
      labels={{ primary: t("nav.primary"), footer: t("nav.account"), badge: (n) => t("nav.badge", { n }) }}
    />
  );
}
