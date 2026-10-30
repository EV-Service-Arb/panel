"use client";

import { Skeleton } from "@evinvest/uikit";
import { ACCOUNT_SECTIONS } from "@evinvest/concierge";
import { AccountPage } from "@evinvest/concierge/react";
import { useRouter, useSearchParams } from "next/navigation";
import { useEffect, useState } from "react";

import { panelRouteFor } from "@/features/account-chip";
import { useSignOut } from "@/features/sign-out";
import { platformMfe } from "@/shared/api";
import { ROUTES } from "@/shared/config/routes";
import { useLocale, useT } from "@/shared/i18n";
import { EmptyState } from "@/shared/ui/empty-state";
import { ScreenFrame } from "@/shared/ui/screen-frame";

const TIMEOUT_MS = 8000;

/**
 * Concierge's account element, inline. On this origin it shows who is signed in; what acts on
 * the platform session (security, notifications) it links to evinvest.ltd's account page.
 */
export function AccountView() {
  const t = useT();
  const locale = useLocale();
  const router = useRouter();
  const signOut = useSignOut();
  const raw = useSearchParams().get("section");
  const section = ACCOUNT_SECTIONS.find((s) => s === raw);
  // undefined while the registry is asked, null when it gave no account entry.
  const [scriptUrl, setScriptUrl] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    let stale = false;
    void platformMfe.scriptUrl("account", TIMEOUT_MS).then((url) => {
      if (!stale) setScriptUrl(url);
    });
    return () => {
      stale = true;
    };
  }, []);

  const loading = <Skeleton className="h-64 w-full" />;
  const onNavigate = (href: string): boolean => {
    const route = panelRouteFor(href);
    if (route !== null) router.push(route);
    return route !== null;
  };
  return (
    <ScreenFrame title={t("nav.account")} back={ROUTES.more} width="content">
      {scriptUrl === undefined ? (
        loading
      ) : scriptUrl === null ? (
        <EmptyState title={t("account.unavailable")} />
      ) : (
        <AccountPage
          // The element reads `<html lang>` only when it renders.
          key={locale}
          scriptUrl={scriptUrl}
          {...(section === undefined ? {} : { section })}
          onSection={(s) => router.replace(`${ROUTES.account}?section=${s}`, { scroll: false })}
          onNavigate={onNavigate}
          onSignOut={signOut}
          // useSignOut ends the platform session itself, after the panel's.
          platformSignOut={false}
          fallback={loading}
        />
      )}
    </ScreenFrame>
  );
}
