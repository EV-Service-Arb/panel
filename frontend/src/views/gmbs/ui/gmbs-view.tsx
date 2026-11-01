"use client";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  PageFrame,
  Settled,
  Skeleton,
  buttonVariants,
} from "@evinvest/uikit";
import { ExternalLink, Trash2 } from "lucide-react";

import { type ArchiveTag, type Gmail, type Gmb, type SiteTag, fetchArchive, fetchGmails, fetchSiteTags, removeGmail, removeGmb } from "@/entities/gmb";
import { may, useMe } from "@/entities/session";
import { AddGmailButton } from "@/features/add-gmail";
import { AddGmbButton } from "@/features/add-gmb";
import { LinkPlaceButton } from "@/features/link-gmb-place";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize } from "@/shared/ui/touch";

/** The caller's gmails and their GMBs; each service that knows a GMB adds its tags, read from that service. */
export function GmbsView() {
  const t = useT();
  const gmails = useResource("gmbs", fetchGmails, "gmbs", { focus: true });
  const archive = useResource("gmbs.archive", fetchArchive, "gmbs.archive", { focus: true });
  const sites = useResource("gmbs.sites", fetchSiteTags, "gmbs.sites", { focus: true });
  const tags: Tags = {
    archive: archive.status === "ok" ? (archive.data?.gmbs ?? null) : null,
    sites: sites.status === "ok" ? sites.data : null,
    reloadSites: sites.reload,
  };

  return (
    <PageFrame breakpoint="md" title={t("gmbs.title")} description={t("gmbs.description")} actions={<AddGmailButton onAdded={gmails.reload} />}>
      <Settled loading={gmails.status === "loading"} skeleton={<Skeleton className="h-40 w-full" />}>
        {gmails.status === "error" && <ErrorState failure={gmails.failure} onRetry={gmails.reload} />}
        {gmails.status === "ok" && gmails.data.length === 0 && <EmptyState title={t("gmbs.empty")} description={t("gmbs.empty.body")} />}
        {gmails.status === "ok" && gmails.data.map((g) => <GmailCard key={g.id} gmail={g} tags={tags} onChanged={gmails.reload} />)}
      </Settled>
    </PageFrame>
  );
}

/** A service's tags: `null` while it has not answered, or did not (down, not permitted). */
interface Tags {
  archive: Map<number, ArchiveTag> | null;
  sites: Map<number, SiteTag> | null;
  reloadSites: () => void;
}

function GmailCard({ gmail, tags, onChanged }: { gmail: Gmail; tags: Tags; onChanged: () => void }) {
  const t = useT();
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="flex flex-row items-center gap-3 px-4">
        <CardTitle className="min-w-0 flex-1 truncate text-base">{gmail.gmail}</CardTitle>
        <span className="text-sm text-ink-soft tabular-nums">{t("gmbs.count", { n: gmail.gmbs.length })}</span>
        <AddGmbButton gmailId={gmail.id} onAdded={onChanged} />
        <Remove title={t("gmbs.gmail.remove.title", { gmail: gmail.gmail })} body={t("gmbs.gmail.remove.body")} remove={() => removeGmail(gmail.id)} onRemoved={onChanged} />
      </CardHeader>
      <CardContent className="px-4">
        {gmail.gmbs.length === 0 ? (
          <p className="text-sm text-ink-soft">{t("gmbs.gmail.empty")}</p>
        ) : (
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {gmail.gmbs.map((b) => (
              <GmbCard key={b.id} gmb={b} tags={tags} onChanged={onChanged} />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

function GmbCard({ gmb, tags, onChanged }: { gmb: Gmb; tags: Tags; onChanged: () => void }) {
  const t = useT();
  const me = useMe();
  const reviews = tags.archive?.get(gmb.id);
  const site = tags.sites?.get(gmb.id);
  return (
    <li className="flex flex-col gap-2 rounded-md border border-border p-3">
      <div className="flex items-start gap-2">
        <div className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-sm font-medium text-ink">{gmb.name ?? gmb.place_id}</span>
          {gmb.address && <span className="truncate text-xs text-ink-soft">{gmb.address}</span>}
        </div>
        <Remove title={t("gmbs.gmb.remove.title", { name: gmb.name ?? gmb.place_id })} body={t("gmbs.gmb.remove.body")} remove={() => removeGmb(gmb.id)} onRemoved={onChanged} />
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        {reviews && (
          <Badge asChild variant="outline" className={reviews.tracked ? undefined : "text-ink-soft"}>
            <a href={`/review_archive/gmbs/${gmb.id}`}>
              {!reviews.tracked
                ? t("gmbs.tag.reviews.off")
                : reviews.rating === null || reviews.reviews === null
                  ? t("gmbs.tag.reviews.unrated")
                  : t("gmbs.tag.reviews", { rating: reviews.rating.toFixed(1), n: reviews.reviews })}
            </a>
          </Badge>
        )}
        {site && (
          <Badge asChild variant="outline">
            <a href={`/sites/places?${new URLSearchParams({ brand: site.brand, location: site.location })}`}>{t("gmbs.tag.site", { place: `${site.brand}/${site.location}` })}</a>
          </Badge>
        )}
        {tags.sites && may(me, "sa:sites:work:places:edit") && <LinkPlaceButton gmbId={gmb.id} linked={tags.sites} onDone={tags.reloadSites} />}
        <Badge asChild variant="outline">
          <a href={`https://www.google.com/maps/place/?q=place_id:${encodeURIComponent(gmb.place_id)}`} target="_blank" rel="noreferrer">
            {t("gmbs.tag.maps")}
            <ExternalLink aria-hidden className="size-3" />
          </a>
        </Badge>
      </div>
    </li>
  );
}

function Remove({ title, body, remove, onRemoved }: { title: string; body: string; remove: () => Promise<void>; onRemoved: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const act = async () => {
    try {
      await remove();
      onRemoved();
    } catch (e) {
      notifyFailure(e, t);
    }
  };
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button variant="ghost" size={button("sm")} icon aria-label={title}>
          <Trash2 aria-hidden />
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{body}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel className={buttonVariants({ variant: "outline", size: button() })}>{t("move.cancel")}</AlertDialogCancel>
          <AlertDialogAction className={buttonVariants({ variant: "destructive", size: button() })} onClick={() => void act()}>
            {t("gmbs.remove")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
