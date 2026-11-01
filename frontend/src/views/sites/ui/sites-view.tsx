"use client";

import { useT } from "@/shared/i18n";

import { SitesElement } from "@/features/sites-element";

/** `/sites/*`: the element routes its views under `base`. */
export function SitesView() {
  const t = useT();
  return <SitesElement tag="mfe-sites-panel" className="h-full" fallback={<p className="p-6 text-muted-foreground">{t("sites.loading")}</p>} />;
}
