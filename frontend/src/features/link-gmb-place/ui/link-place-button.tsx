"use client";

import { Button, CommandDialog, CommandEmpty, CommandInput, CommandItem, CommandList } from "@evinvest/uikit";
import { Link2 } from "lucide-react";
import { useState } from "react";

import { type SiteTag, fetchSitesPlaces, linkPlace } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize } from "@/shared/ui/touch";

/** Which of sites' places is this GMB's landing; `linked` holds every GMB's link, so a place another GMB has is shown taken. */
export function LinkPlaceButton({ gmbId, linked, onDone }: { gmbId: number; linked: ReadonlyMap<number, SiteTag>; onDone: () => void }) {
  const t = useT();
  const button = useButtonSize();
  const [open, setOpen] = useState(false);
  const own = linked.get(gmbId);

  const act = async (write: () => Promise<void>) => {
    try {
      await write();
      setOpen(false);
      onDone();
    } catch (e) {
      notifyFailure(e, t);
    }
  };

  const label = own ? t("gmbs.tag.site", { place: `${own.brand}/${own.location}` }) : t("gmbs.tag.site.link");
  return (
    <>
      <Button variant="ghost" size={button("sm")} icon aria-label={label} title={label} className={own ? "text-ink" : "text-ink-soft"} onClick={() => setOpen(true)}>
        <Link2 aria-hidden />
      </Button>
      {open && (
        <Picker
          gmbId={gmbId}
          linked={linked}
          onClose={() => setOpen(false)}
          onPick={(brand, location) => void act(() => linkPlace(brand, location, gmbId))}
          {...(own && { onUnlink: () => void act(() => linkPlace(own.brand, own.location, null)) })}
        />
      )}
    </>
  );
}

function Picker({
  gmbId,
  linked,
  onClose,
  onPick,
  onUnlink,
}: {
  gmbId: number;
  linked: ReadonlyMap<number, SiteTag>;
  onClose: () => void;
  onPick: (brand: string, location: string) => void;
  onUnlink?: () => void;
}) {
  const t = useT();
  const places = useResource("gmbs.link.places", fetchSitesPlaces);
  const holder = new Map([...linked].map(([id, tag]) => [`${tag.brand}/${tag.location}`, id]));
  return (
    <CommandDialog open onOpenChange={(o) => !o && onClose()}>
      <CommandInput placeholder={t("gmbs.link.search")} />
      <CommandList>
        <CommandEmpty>{places.status === "loading" ? "…" : t("gmbs.link.none")}</CommandEmpty>
        {onUnlink && <CommandItem value={t("gmbs.link.unlink")} onSelect={onUnlink}>{t("gmbs.link.unlink")}</CommandItem>}
        {places.status === "error" && <p className="px-3 py-2 text-sm text-ink-soft">{t("gmbs.tags.failed", { service: t("nav.sites") })}</p>}
        {places.status === "ok" &&
          places.data.map((p) => {
            const key = `${p.brand}/${p.location}`;
            const other = holder.get(key);
            const taken = other !== undefined && other !== gmbId;
            return (
              <CommandItem key={key} value={key} disabled={p.withdrawn || taken} onSelect={() => onPick(p.brand, p.location)}>
                <span className="flex-1">{key}</span>
                {p.withdrawn && <span className="text-xs text-ink-soft">{t("gmbs.link.withdrawn")}</span>}
                {taken && <span className="text-xs text-ink-soft">{t("gmbs.link.taken")}</span>}
              </CommandItem>
            );
          })}
      </CommandList>
    </CommandDialog>
  );
}
