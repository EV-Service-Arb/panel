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
  Skeleton,
  Switch,
  buttonVariants,
  cn,
} from "@evinvest/uikit";
import { ChevronRight, Trash2 } from "lucide-react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";

import { type Gmail, type Gmb, type SiteTag, fetchGmails, fetchSiteTags, removeGmail, removeGmb, switchGmbs } from "@/entities/gmb";
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

/** The review archive's view of what the pathname points at, if any. */
function pointedAt(pathname: string): { gmail: number } | { gmb: number } | null {
  const m = /^\/review_archive\/(gmails|gmbs)\/(\d+)/.exec(pathname);
  if (m === null) return null;
  return m[1] === "gmails" ? { gmail: Number(m[2]) } : { gmb: Number(m[2]) };
}

/**
 * The caller's managing gmails, each opening to its GMBs, with their switches: the outline every
 * page sits beside. A GMB's switch is the conductor's; the services inherit it.
 */
export function GmbTree({ onNavigate }: { onNavigate?: () => void }) {
  const t = useT();
  const me = useMe();
  const gmails = useResource("gmbs", fetchGmails, "gmbs", { focus: true });
  const linking = may(me, "sa:sites:work:places:edit");
  const sites = useResource("gmbs.sites", () => (linking ? fetchSiteTags() : Promise.resolve(null)), "gmbs.sites", { focus: true });
  const linked = sites.status === "ok" ? sites.data : null;
  const at = pointedAt(usePathname());
  const [open, setOpen] = useState<ReadonlySet<number>>(new Set());

  const toggle = (id: number) =>
    setOpen((o) => {
      const next = new Set(o);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const flip = async (ids: number[], enabled: boolean) => {
    try {
      await switchGmbs(ids, enabled);
    } catch (e) {
      notifyFailure(e, t);
    }
    gmails.reload();
  };

  const all = gmails.status === "ok" ? gmails.data.flatMap((g) => g.gmbs) : [];
  return (
    <section aria-label={t("gmbs.title")} className="flex flex-col gap-0.5">
      <div className="flex items-center gap-1 px-2 pb-1 text-xs font-medium tracking-wide text-ink-soft uppercase">
        <Link href="/" onClick={() => onNavigate?.()} className="rounded-md px-1 py-0.5 hover:bg-hover/50">
          {t("gmbs.title")}
        </Link>
        <InfoTip>
          <InfoTipTrigger label={t("gmbs.what")} />
          <InfoTipContent className="font-normal tracking-normal normal-case">{t("gmbs.gmail.hint")}</InfoTipContent>
        </InfoTip>
        <span className="flex-1" />
        <Switch aria-label={t("gmbs.switch.all")} checked={all.length > 0 && all.every((b) => b.enabled)} disabled={all.length === 0} onCheckedChange={(on) => void flip(all.map((b) => b.id), on)} />
      </div>
      {gmails.status === "loading" && <Skeleton className="mx-2 h-24" />}
      {gmails.status === "error" && <ErrorState failure={gmails.failure} onRetry={gmails.reload} />}
      {gmails.status === "ok" &&
        gmails.data.map((g) => (
          <GmailRow
            key={g.id}
            gmail={g}
            open={open.has(g.id) || (at !== null && "gmb" in at && g.gmbs.some((b) => b.id === at.gmb))}
            current={at !== null && "gmail" in at && at.gmail === g.id}
            currentGmb={at !== null && "gmb" in at ? at.gmb : null}
            linked={linked}
            onToggle={() => toggle(g.id)}
            onFlip={flip}
            onChanged={() => {
              gmails.reload();
              sites.reload();
            }}
            {...(onNavigate && { onNavigate })}
          />
        ))}
      <AddGmailButton onAdded={gmails.reload} />
    </section>
  );
}

function GmailRow({
  gmail,
  open,
  current,
  currentGmb,
  linked,
  onToggle,
  onFlip,
  onChanged,
  onNavigate,
}: {
  gmail: Gmail;
  open: boolean;
  current: boolean;
  currentGmb: number | null;
  linked: Map<number, SiteTag> | null;
  onToggle: () => void;
  onFlip: (ids: number[], enabled: boolean) => Promise<void>;
  onChanged: () => void;
  onNavigate?: () => void;
}) {
  const t = useT();
  const on = gmail.gmbs.length > 0 && gmail.gmbs.every((b) => b.enabled);
  return (
    <div className="flex flex-col">
      <div className={cn(ROW, current && "bg-hover/50")}>
        <button type="button" onClick={onToggle} aria-expanded={open} aria-label={t(open ? "gmbs.collapse" : "gmbs.expand", { gmail: gmail.gmail })} className="rounded p-1 text-ink-soft hover:text-ink">
          <ChevronRight aria-hidden className={cn("size-3.5 transition-transform", open && "rotate-90")} />
        </button>
        <Link
          href={`${ROUTES.reviewArchive}gmails/${gmail.id}`}
          onClick={() => onNavigate?.()}
          aria-current={current ? "page" : undefined}
          className={cn("min-w-0 flex-1 truncate py-1.5", on ? "text-ink" : "text-ink-soft")}
          title={gmail.gmail}
        >
          {gmail.gmail}
        </Link>
        <span className={cn("flex items-center", ON_HOVER)}>
          <AddGmbButton gmailId={gmail.id} onAdded={onChanged} />
          <Remove title={t("gmbs.gmail.remove.title", { gmail: gmail.gmail })} body={t("gmbs.gmail.remove.body")} remove={() => removeGmail(gmail.id)} onRemoved={onChanged} />
        </span>
        <span className="text-xs text-ink-soft tabular-nums">{gmail.gmbs.length}</span>
        <Switch aria-label={t("gmbs.switch", { name: gmail.gmail })} checked={on} disabled={gmail.gmbs.length === 0} onCheckedChange={(v) => void onFlip(gmail.gmbs.map((b) => b.id), v)} />
      </div>
      {open && (
        <ul className="ml-5 flex flex-col border-l border-border pl-1">
          {gmail.gmbs.length === 0 && <li className="px-2 py-1.5 text-xs text-ink-soft">{t("gmbs.gmail.empty")}</li>}
          {gmail.gmbs.map((b) => (
            <GmbRow key={b.id} gmb={b} current={b.id === currentGmb} linked={linked} onFlip={onFlip} onChanged={onChanged} {...(onNavigate && { onNavigate })} />
          ))}
        </ul>
      )}
    </div>
  );
}

function GmbRow({
  gmb,
  current,
  linked,
  onFlip,
  onChanged,
  onNavigate,
}: {
  gmb: Gmb;
  current: boolean;
  linked: Map<number, SiteTag> | null;
  onFlip: (ids: number[], enabled: boolean) => Promise<void>;
  onChanged: () => void;
  onNavigate?: () => void;
}) {
  const t = useT();
  const name = gmb.name ?? gmb.place_id;
  return (
    <li className={cn(ROW, "pl-2", current && "bg-hover/50")}>
      <Link
        href={`${ROUTES.reviewArchive}gmbs/${gmb.id}`}
        onClick={() => onNavigate?.()}
        aria-current={current ? "page" : undefined}
        className={cn("min-w-0 flex-1 truncate py-1.5", gmb.enabled ? "text-ink" : "text-ink-soft")}
        title={gmb.address ? `${name}\n${gmb.address}` : name}
      >
        {name}
      </Link>
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
