"use client";

import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, buttonVariants } from "@evinvest/uikit";
import { X } from "lucide-react";
import Link from "next/link";

import { type Listed, fetchSiteTags } from "@/entities/gmb";
import { seesSites, useMe } from "@/entities/session";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { useButtonSize } from "@/shared/ui/touch";

/** A GMB's card over the map: what the conductor knows of it, and where its services show it. */
export function Peek({ listed: { gmb, gmail }, onClose }: { listed: Listed; onClose: () => void }) {
  const t = useT();
  const me = useMe();
  const button = useButtonSize();
  const sites = useResource("gmbs.sites", () => (seesSites(me) ? fetchSiteTags() : Promise.resolve(null)), "gmbs.sites");
  const site = sites.status === "ok" ? sites.data?.get(gmb.id) : undefined;
  const name = gmb.name ?? gmb.place_id;
  return (
    <Card className="absolute top-4 right-4 w-80 max-w-[calc(100%-2rem)] gap-3 py-4 shadow-overlay">
      <CardHeader className="flex items-start gap-2 px-4">
        <div className="min-w-0 flex-1">
          <CardTitle className="truncate" title={name}>
            {name}
          </CardTitle>
          <CardDescription>{[gmb.address, gmail.gmail].filter(Boolean).join(" · ")}</CardDescription>
        </div>
        <Button variant="ghost" size={button("sm")} icon aria-label={t("home.peek.close")} title={t("home.peek.close")} onClick={onClose}>
          <X aria-hidden />
        </Button>
      </CardHeader>
      <CardContent className="flex flex-wrap gap-2 px-4">
        {!gmb.enabled && <p className="w-full text-sm text-ink-soft">{t("home.peek.off")}</p>}
        <Link href={`${ROUTES.reviewArchive}gmbs/${gmb.id}`} className={buttonVariants({ variant: "outline", size: button("sm") })}>
          {t("home.peek.reviews")}
        </Link>
        {site && (
          <Link href={`${ROUTES.sites}places`} className={buttonVariants({ variant: "outline", size: button("sm") })}>
            {t("gmbs.tag.site", { place: `${site.brand}/${site.location}` })}
          </Link>
        )}
      </CardContent>
    </Card>
  );
}
