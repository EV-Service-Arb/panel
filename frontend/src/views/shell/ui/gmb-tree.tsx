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
  Button,
  InfoTip,
  InfoTipContent,
  InfoTipTrigger,
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Skeleton,
  Switch,
  buttonVariants,
  cn,
} from "@evinvest/uikit";
import { ChevronDown, Mail, Search, Trash2 } from "lucide-react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";

import {
  type Gmail,
  type Listed,
  type SiteTag,
  byView,
  chooseGmailScope,
  fetchSiteTags,
  narrowed,
  removeGmail,
  removeGmb,
  setOnMap,
  switchGmbs,
  useGmailScope,
  useGmails,
  useOnMap,
} from "@/entities/gmb";
import { may, useMe } from "@/entities/session";
import { AddGmailButton } from "@/features/add-gmail";
import { AddGmbButton } from "@/features/add-gmb";
import { LinkPlaceButton } from "@/features/link-gmb-place";
import { ROUTES } from "@/shared/config/routes";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { ErrorState } from "@/shared/ui/error-state";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize } from "@/shared/ui/touch";

const ROW = "group flex min-w-0 items-center gap-1 rounded-md pr-1 text-sm hover:bg-hover/50";
/** Shown on the row's hover or focus only, so the names keep the width. */
const ON_HOVER = "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100";
const HEADING = "flex items-center gap-1 px-2 pt-2 pb-1 text-xs font-medium tracking-wide text-ink-soft uppercase";

/** The GMB the review archive's page is on, if any. */
function pointedAt(pathname: string): number | null {
  const m = /^\/review_archive\/gmbs\/(\d+)/.exec(pathname);
  return m === null ? null : Number(m[1]);
}

/**
 * The caller's GMBs, under a gmail scope every app shares: the outline every page sits beside. On
 * the map, split by its viewport, and a row opens the GMB's card; elsewhere a row opens the GMB in
 * the review archive. A GMB's switch is the conductor's; the services inherit it.
 */
export function GmbTree({ onNavigate }: { onNavigate?: () => void }) {
  const t = useT();
  const me = useMe();
  const gmails = useGmails();
  const scope = useGmailScope();
  const { inView, hovered, peeked } = useOnMap();
  const pathname = usePathname();
  const linking = may(me, "sa:sites:work:places:edit");
  const sites = useResource("gmbs.sites", () => (linking ? fetchSiteTags() : Promise.resolve(null)), "gmbs.sites", { focus: true });
  const linked = sites.status === "ok" ? sites.data : null;
  const [filter, setFilter] = useState("");

  // a scoped gmail removed, here or in another tab: back to all of them
  const known = gmails.status === "ok" ? gmails.data : null;
  useEffect(() => {
    if (known !== null && scope !== null && !known.some((g) => g.id === scope)) chooseGmailScope(null);
  }, [known, scope]);

  const flip = async (ids: number[], enabled: boolean) => {
    try {
      await switchGmbs(ids, enabled);
    } catch (e) {
      notifyFailure(e, t);
    }
    gmails.reload();
  };
  const changed = () => {
    gmails.reload();
    sites.reload();
  };

  if (gmails.status === "loading") return <Skeleton className="mx-2 h-24" />;
  if (gmails.status === "error") return <ErrorState failure={gmails.failure} onRetry={gmails.reload} />;
  const scoped = gmails.data.find((g) => g.id === scope) ?? null;
  const listed = narrowed(gmails.data, scoped?.id ?? null, filter);
  const onMap = pathname === "/" && inView !== null;
  const row = (l: Listed) => (
    <GmbRow
      key={l.gmb.id}
      listed={l}
      showGmail={scoped === null}
      onMap={onMap}
      lit={l.gmb.id === hovered || l.gmb.id === peeked}
      current={l.gmb.id === pointedAt(pathname)}
      linked={linked}
      onFlip={flip}
      onChanged={changed}
      {...(onNavigate && { onNavigate })}
    />
  );
  const split = onMap ? byView(listed, inView) : null;

  return (
    <section aria-label={t("gmbs.title")} className="flex flex-col gap-1">
      <Scope gmails={gmails.data} scoped={scoped} onFlip={flip} onChanged={changed} />
      <InputGroup className="mx-0.5 w-auto">
        <InputGroupAddon>
          <Search aria-hidden />
        </InputGroupAddon>
        <InputGroupInput type="search" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={t("gmbs.filter")} aria-label={t("gmbs.filter")} />
      </InputGroup>
      {split ? (
        <>
          <h3 className={HEADING}>
            {t("gmbs.inView")} <span className="tabular-nums">{split.inView.length}</span>
          </h3>
          <ul className="flex flex-col">{split.inView.map(row)}</ul>
          <h3 className={HEADING}>
            {t("gmbs.elsewhere")} <span className="tabular-nums">{split.elsewhere.length}</span>
          </h3>
          <ul className="flex flex-col">{split.elsewhere.map(row)}</ul>
        </>
      ) : (
        <ul className="flex flex-col pt-1">{listed.map(row)}</ul>
      )}
      {listed.length === 0 && <p className="px-2 py-1.5 text-xs text-ink-soft">{t(filter.trim() === "" ? "gmbs.none" : "gmbs.none.filtered")}</p>}
      {scoped && (
        <div className="flex items-center gap-1 px-1 text-sm text-ink-soft">
          <AddGmbButton gmailId={scoped.id} onAdded={changed} />
          {t("gmbs.gmb.add")}
        </div>
      )}
    </section>
  );
}

/** Which gmail's GMBs every app shows: all, or one; and each gmail's switch, count and actions. */
function Scope({ gmails, scoped, onFlip, onChanged }: { gmails: Gmail[]; scoped: Gmail | null; onFlip: (ids: number[], enabled: boolean) => Promise<void>; onChanged: () => void }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const total = gmails.reduce((n, g) => n + g.gmbs.length, 0);
  const choose = (id: number | null) => {
    chooseGmailScope(id);
    setOpen(false);
  };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger className="mx-0.5 flex items-center gap-2 rounded-md border border-border px-2.5 py-2 text-sm text-ink outline-none hover:bg-hover/50 focus-visible:ring-2 focus-visible:ring-ring">
        <Mail aria-hidden className="size-4 text-ink-soft" />
        <span className="min-w-0 flex-1 truncate text-left">{scoped ? scoped.gmail : t("gmbs.scope.all")}</span>
        <span className="text-xs text-ink-soft tabular-nums">{scoped ? scoped.gmbs.length : gmails.length}</span>
        <ChevronDown aria-hidden className="size-4 text-ink-soft" />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-1">
        <div className={cn(HEADING, "pt-1")}>
          {t("gmbs.title")}
          <InfoTip>
            <InfoTipTrigger label={t("gmbs.what")} />
            <InfoTipContent className="font-normal tracking-normal normal-case">{t("gmbs.gmail.hint")}</InfoTipContent>
          </InfoTip>
          <span className="flex-1" />
          <span className="tabular-nums">{t("gmbs.scope.counts", { gmails: gmails.length, gmbs: total })}</span>
        </div>
        <button type="button" onClick={() => choose(null)} aria-current={scoped === null ? "true" : undefined} className={cn(ROW, "w-full py-1.5 pl-2", scoped === null && "bg-hover/50")}>
          <span className="flex-1 text-left">{t("gmbs.scope.all")}</span>
          <span className="text-xs text-ink-soft tabular-nums">{total}</span>
        </button>
        {gmails.map((g) => {
          const on = g.gmbs.length > 0 && g.gmbs.every((b) => b.enabled);
          return (
            <div key={g.id} className={cn(ROW, g.id === scoped?.id && "bg-hover/50")}>
              <button
                type="button"
                onClick={() => choose(g.id)}
                aria-current={g.id === scoped?.id ? "true" : undefined}
                className={cn("min-w-0 flex-1 truncate py-1.5 pl-2 text-left", on ? "text-ink" : "text-ink-soft")}
                title={g.gmail}
              >
                {g.gmail}
              </button>
              <span className={cn("flex items-center", ON_HOVER)}>
                <AddGmbButton gmailId={g.id} onAdded={onChanged} />
                <Remove title={t("gmbs.gmail.remove.title", { gmail: g.gmail })} body={t("gmbs.gmail.remove.body")} remove={() => removeGmail(g.id)} onRemoved={onChanged} />
              </span>
              <span className="text-xs text-ink-soft tabular-nums">{g.gmbs.length}</span>
              <Switch aria-label={t("gmbs.switch", { name: g.gmail })} checked={on} disabled={g.gmbs.length === 0} onCheckedChange={(v) => void onFlip(g.gmbs.map((b) => b.id), v)} />
            </div>
          );
        })}
        <div className="border-t border-border pt-1">
          <AddGmailButton onAdded={onChanged} />
        </div>
      </PopoverContent>
    </Popover>
  );
}

function GmbRow({
  listed: { gmb, gmail },
  showGmail,
  onMap,
  lit,
  current,
  linked,
  onFlip,
  onChanged,
  onNavigate,
}: {
  listed: Listed;
  showGmail: boolean;
  onMap: boolean;
  lit: boolean;
  current: boolean;
  linked: Map<number, SiteTag> | null;
  onFlip: (ids: number[], enabled: boolean) => Promise<void>;
  onChanged: () => void;
  onNavigate?: () => void;
}) {
  const t = useT();
  const name = gmb.name ?? gmb.place_id;
  const label = (
    <>
      <span className={cn("block truncate", gmb.enabled ? "text-ink" : "text-ink-soft")}>{name}</span>
      <span className="block truncate text-xs text-ink-soft">{[gmb.address, showGmail ? gmail.gmail : null].filter(Boolean).join(" · ")}</span>
    </>
  );
  const body = "min-w-0 flex-1 py-1.5 pl-2 text-left";
  return (
    <li
      className={cn(ROW, (current || lit) && "bg-hover/50")}
      onMouseEnter={() => setOnMap({ hovered: gmb.id })}
      onMouseLeave={() => setOnMap({ hovered: null })}
    >
      <span aria-hidden className={cn("ml-1 size-2 shrink-0 rounded-full", gmb.enabled ? "bg-positive" : "bg-ink-soft", gmb.location === null && "opacity-40")} />
      {onMap ? (
        <button type="button" className={body} title={gmb.address ? `${name}\n${gmb.address}` : name} onClick={() => setOnMap({ peeked: gmb.id })}>
          {label}
        </button>
      ) : (
        <Link
          href={`${ROUTES.reviewArchive}gmbs/${gmb.id}`}
          onClick={() => onNavigate?.()}
          aria-current={current ? "page" : undefined}
          className={body}
          title={gmb.address ? `${name}\n${gmb.address}` : name}
        >
          {label}
        </Link>
      )}
      <span className={cn("flex items-center", ON_HOVER)}>
        {linked && <LinkPlaceButton gmbId={gmb.id} linked={linked} onDone={onChanged} />}
        <Remove title={t("gmbs.gmb.remove.title", { name })} body={t("gmbs.gmb.remove.body")} remove={() => removeGmb(gmb.id)} onRemoved={onChanged} />
      </span>
      <Switch aria-label={t("gmbs.switch", { name })} checked={gmb.enabled} onCheckedChange={(v) => void onFlip([gmb.id], v)} />
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
        <Button variant="ghost" size={button("sm")} icon aria-label={title} title={title}>
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
