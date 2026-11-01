"use client";

import { Settled, Skeleton } from "@evinvest/uikit";
import { useCallback, useState } from "react";

import { type PricingItem, fetchPricing, followsPricing, pricingSavedSince } from "@/entities/pricing";
import { may, useMe } from "@/entities/session";
import { useResource } from "@/shared/lib/use-resource";
import { ErrorState } from "@/shared/ui/error-state";

import { pinFrom } from "../model/pin";
import { EditWorkspace } from "./edit-workspace";
import { ReadWorkspace } from "./read-workspace";

/**
 * One brand's pricing. The editor works from `pinned` — the pricing as it was
 * when the draft started — while the read underneath follows live changes:
 * one's own save pins what it returned, someone else's shows up as `fresher`
 * and the editor decides what to do with it.
 */
export function BrandPricing({ brand, today }: { brand: string; today: string }) {
  const me = useMe();
  const [version, setVersion] = useState(0);
  const [pinned, setPinned] = useState<PricingItem | null>(null);
  // The pricing the person chose to load: its workspace mounts with focus on the status, not on <body>.
  const [chosen, setChosen] = useState<PricingItem | null>(null);
  const data = useResource(`pricing:${brand}:${version}`, () => fetchPricing(brand), `pricing:${brand}`, { live: followsPricing(brand) });
  const latest = data.status === "ok" ? data.data : null;
  const toPin = pinFrom(pinned, latest, data.fresh);
  if (toPin !== null) setPinned(toPin);

  /** After a write: `item` is what the server now holds; without one, read it again and pin that read once it lands. */
  const written = useCallback((item?: PricingItem) => {
    setPinned(item ?? null);
    setVersion((v) => v + 1);
  }, []);
  const takeFresh = useCallback((item: PricingItem, wasChosen: boolean) => {
    setPinned(item);
    setChosen(wasChosen ? item : null);
  }, []);

  return (
    <Settled loading={data.status === "loading"} skeleton={<Skeleton className="h-96 w-full" />}>
      {data.status === "error" && <ErrorState failure={data.failure} onRetry={data.reload} />}
      {latest !== null &&
        (may(me, "sa:sites:work:pricing:edit") ? (
          <EditWorkspace
            key={pinned?.updated_at ?? "never"}
            base={pinned ?? latest}
            fresher={pinned && pricingSavedSince(latest, pinned) ? latest : null}
            today={today}
            version={version}
            focusStatus={pinned !== null && pinned === chosen}
            onWritten={written}
            onTakeFresh={takeFresh}
          />
        ) : (
          // Nothing typed to protect: a reader sees the newest at once.
          <ReadWorkspace item={latest} version={version} />
        ))}
    </Settled>
  );
}
