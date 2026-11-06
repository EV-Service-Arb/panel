"use client";

import { Button } from "@evinvest/uikit";
import { importLibrary } from "@googlemaps/js-api-loader";
import { useEffect, useRef, useState } from "react";

import { type Gmb, type LatLng, placeGmb, setOnMap, useGmails } from "@/entities/gmb";
import { useT } from "@/shared/i18n";
import { notifyFailure } from "@/shared/ui/notify";
import { useButtonSize } from "@/shared/ui/touch";

/** Over the map's bottom edge while a GMB's point is placed: a place search that moves the map and the pin, and the point set where the pin is. */
export function PlacingBar({ gmb, map, at, onMove }: { gmb: Gmb; map: google.maps.Map; at: LatLng; onMove: (at: LatLng) => void }) {
  const t = useT();
  const button = useButtonSize();
  const gmails = useGmails();
  const search = useRef<HTMLDivElement>(null);
  const [busy, setBusy] = useState(false);
  // the search box is built once per map; it reads these as they are now
  const latest = useRef({ onMove, t });
  useEffect(() => {
    latest.current = { onMove, t };
  });

  useEffect(() => {
    const box = search.current;
    if (box === null) throw new Error("PlacingBar: its search box is not rendered");
    let live = true;
    let element: google.maps.places.PlaceAutocompleteElement | null = null;
    void importLibrary("places").then(({ PlaceAutocompleteElement }) => {
      if (!live) return;
      element = new PlaceAutocompleteElement({});
      element.className = "w-full";
      element.addEventListener("gmp-select", (e) => {
        const place = e.placePrediction.toPlace();
        place.fetchFields({ fields: ["location", "viewport"] }).then(
          () => {
            if (!place.location) throw new Error(`Places gave ${place.id} no location`);
            if (place.viewport) map.fitBounds(place.viewport);
            else map.panTo(place.location);
            latest.current.onMove(place.location.toJSON());
          },
          (failed: unknown) => notifyFailure(failed, latest.current.t),
        );
      });
      box.appendChild(element);
    });
    return () => {
      live = false;
      element?.remove();
    };
  }, [map]);

  const set = async () => {
    setBusy(true);
    try {
      await placeGmb(gmb.id, at);
      gmails.reload();
      setOnMap({ placing: null, peeked: gmb.id });
    } catch (e) {
      notifyFailure(e, t);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="absolute inset-x-4 bottom-4 flex flex-wrap items-center gap-2 rounded-lg border border-border bg-background p-3 shadow-overlay">
      <p className="min-w-0 flex-1 basis-60 text-sm text-ink">{t("home.placing", { name: gmb.name ?? gmb.place_id })}</p>
      <div ref={search} className="min-w-0 flex-1 basis-60" />
      <Button variant="outline" size={button()} onClick={() => setOnMap({ placing: null })}>
        {t("move.cancel")}
      </Button>
      <Button size={button()} disabled={busy} onClick={() => void set()}>
        {t("home.placing.set")}
      </Button>
    </div>
  );
}
