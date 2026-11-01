"use client";

import { Settled, Skeleton } from "@evinvest/uikit";
import { useState } from "react";

import { fetchFunnelByLocation } from "@/entities/funnel";
import { type PlaceKey, fetchPlaces } from "@/entities/place";
import { may, useMe } from "@/entities/session";
import { fetchSources } from "@/entities/source";
import { AddPlaceButton } from "@/features/add-place";
import { type Period, rangeOf } from "@/features/funnel-filters";
import { useT } from "@/shared/i18n";
import { useLastNonNull } from "@/shared/lib/use-last-non-null";
import { useResource } from "@/shared/lib/use-resource";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";
import { PanelOverlay } from "@/shared/ui/panel-overlay";
import { ScreenFrame } from "@/shared/ui/screen-frame";

import { offeredBrands } from "../model/brands";
import { type PlaceRow, placeRows } from "../model/rows";
import { PlaceCard } from "./place-card";
import { PlaceSettingsPanel } from "./place-settings-panel";

const WINDOW_DAYS: Period = 30;

/**
 * Locations with a mini funnel each, from the backend's per-location funnel,
 * plus what `/places` says of their site data; a card opens that data.
 */
export function PlacesView() {
  const t = useT();
  const me = useMe();
  // Fixed when the screen opens: a range that moved mid-render would refetch in a loop.
  const [range] = useState(() => rangeOf(WINDOW_DAYS, new Date()));
  const [version, setVersion] = useState(0);
  const [open, setOpen] = useState<PlaceKey | null>(null);
  // Title and form stay put while the sheet slides away.
  const shown = useLastNonNull(open);
  const refresh = () => setVersion((v) => v + 1);
  // The cards count leads, and say whether a place has live data or is withdrawn.
  const data = useResource(
    `places:${range.from}:${range.to}:${version}`,
    async () => {
      const [funnel, places, sources] = await Promise.all([
        fetchFunnelByLocation({ ...range, brand: null }),
        fetchPlaces(),
        may(me, "sa:admin:sources:manage") ? fetchSources() : Promise.resolve([]),
      ]);
      return { rows: placeRows(funnel.locations, places), minSample: funnel.min_sample, brands: offeredBrands(places, sources) };
    },
    `places:${range.from}:${range.to}`,
    { live: ["places", "leads", "lead"] },
  );

  return (
    <ScreenFrame
      title={t("places.title")}
      actions={
        may(me, "sa:sites:work:places:edit") && data.status === "ok" ? (
          <AddPlaceButton
            brands={data.data.brands}
            onAdded={(key) => {
              refresh();
              setOpen(key);
            }}
          />
        ) : undefined
      }
    >
      <Settled loading={data.status === "loading"} skeleton={<Skeleton className="h-48 w-full" />} className="flex flex-col gap-4">
        {data.status === "error" && <ErrorState failure={data.failure} onRetry={data.reload} />}
        {data.status === "ok" && <p className="text-sm text-ink-soft">{t("places.window", { days: WINDOW_DAYS, min: data.data.minSample })}</p>}
        {data.status === "ok" && <PlaceGrid rows={data.data.rows} onOpen={setOpen} />}
      </Settled>
      <PanelOverlay
        open={open !== null}
        onOpenChange={(o) => !o && setOpen(null)}
        desktop="sheet"
        title={shown ? t("placeSettings.openFor", { brand: shown.brand, slug: shown.slug }) : ""}
        description={t("placeSettings.description")}
      >
        {shown && <PlaceSettingsPanel key={`${shown.brand}/${shown.slug}`} place={shown} onChanged={refresh} />}
      </PanelOverlay>
    </ScreenFrame>
  );
}

function PlaceGrid({ rows, onOpen }: { rows: PlaceRow[]; onOpen: (key: PlaceKey) => void }) {
  const t = useT();
  if (rows.length === 0) return <EmptyState title={t("places.empty")} description={t("places.empty.body")} />;
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      {rows.map((row) => {
        const slug = row.location;
        return <PlaceCard key={`${row.brand}/${slug ?? ""}`} row={row} onOpen={slug === null ? null : () => onOpen({ brand: row.brand, slug })} />;
      })}
    </div>
  );
}
