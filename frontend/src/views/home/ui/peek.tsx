"use client";

import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, buttonVariants, cn } from "@evinvest/uikit";
import { X } from "lucide-react";
import Link from "next/link";

import { type Listed, issueOf, issueText, useGmbTags } from "@/entities/gmb";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { useButtonSize } from "@/shared/ui/touch";

/** A GMB's card over the map: its numbers, what needs the operator, and where its services show it. */
export function Peek({ listed: { gmb, gmail }, onClose }: { listed: Listed; onClose: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const { tags } = useGmbTags();
  const archive = tags.archive?.get(gmb.id);
  const site = tags.sites?.get(gmb.id);
  const issue = issueOf(gmb, tags);
  const name = gmb.name ?? gmb.place_id;
  const reviewsNote = archive && (archive.suspended > 0 ? t("gmbs.tag.suspended", { n: archive.suspended }) : archive.failing ? t("gmbs.issue.failing") : null);
  const numbers = [
    archive?.rating != null && archive.reviews !== null && t("home.peek.rating", { rating: archive.rating.toFixed(1), n: archive.reviews }),
    site && (site.overdue > 0 ? t("home.peek.leadsOverdue", { n: site.leads_7d, overdue: site.overdue }) : t("home.peek.leads", { n: site.leads_7d })),
  ].filter(Boolean);
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
      <CardContent className="flex flex-col gap-3 px-4">
        <div className="flex flex-col gap-0.5 text-sm">
          {numbers.length > 0 && <p className="text-ink tabular-nums">{numbers.join(" · ")}</p>}
          <p className={cn(issue === null || issue === "off" ? "text-ink-soft" : "font-medium text-accent-error-ink")}>
            {issue === null ? t("home.peek.fine") : issue === "off" ? t("home.peek.off") : issueText(issue, gmb.id, tags, t)}
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Link href={`${ROUTES.reviewArchive}gmbs/${gmb.id}`} className={buttonVariants({ variant: "outline", size: button("sm") })}>
            {reviewsNote ? `${t("home.peek.reviews")} · ${reviewsNote}` : t("home.peek.reviews")}
          </Link>
          {site && (
            <Link href={`${ROUTES.sites}places`} className={buttonVariants({ variant: "outline", size: button("sm") })}>
              {t("gmbs.tag.site", { place: `${site.brand}/${site.location}` })}
              {site.overdue > 0 && ` · ${t("gmbs.tag.overdue", { n: site.overdue })}`}
            </Link>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
