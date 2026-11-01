"use client";

import { seesSites, useMe } from "@/entities/session";
import { RequestAccess } from "@/features/request-access";
import { SitesElement } from "@/features/sites-element";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";

/** `/sites/*`: the element routes its views under `base`; sites refuses its API to whoever holds neither section. */
export function SitesView() {
  const t = useT();
  const me = useMe();
  if (!seesSites(me)) return <RequestAccess need="sa:operator" continueTo={ROUTES.sites} />;
  return <SitesElement tag="mfe-sites-panel" className="h-full" fallback={<p className="p-6 text-muted-foreground">{t("sites.loading")}</p>} />;
}
