"use client";

import { Skeleton, cn } from "@evinvest/uikit";
import { importLibrary } from "@googlemaps/js-api-loader";
import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

import { type Listed, type MapConfig, fetchMapConfig, narrowed, setOnMap, useGmailScope, useGmails, useOnMap } from "@/entities/gmb";
import { takeReturnPage } from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { useTheMap } from "@/shared/map";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";

import { Peek } from "./peek";

/** `/`: the caller's GMBs on a map, unless they come back from the sign-in dialog to the page they were on. */
export function Home() {
  const t = useT();
  const router = useRouter();
  // Taken once: the read clears the key.
  const back = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    back.current ??= takeReturnPage();
    if (back.current !== null && back.current !== "/") router.replace(back.current);
  }, [router]);
  const config = useResource("map", fetchMapConfig);

  return (
    <div className="relative h-[calc(100svh-3.5rem)] min-h-0 flex-1">
      {config.status === "loading" && <Skeleton className="absolute inset-0 rounded-none" />}
      {config.status === "error" && (
        <div className="p-6">
          <ErrorState failure={config.failure} onRetry={config.reload} />
        </div>
      )}
      {config.status === "ok" && config.data === null && <EmptyState className="m-6" title={t("home.noMap")} description={t("home.noMap.why")} />}
      {config.status === "ok" && config.data !== null && <GmbMap config={config.data} />}
    </div>
  );
}

/** Fitted to the GMBs once per page session: the map outlives the page, and so does where it was left. */
let fitted = false;

function GmbMap({ config }: { config: MapConfig }) {
  const gmails = useGmails();
  const scope = useGmailScope();
  const { hovered, peeked } = useOnMap();
  const { host, map } = useTheMap(config.key, config.map_id);
  const listed = gmails.status === "ok" ? narrowed(gmails.data, scope, "") : [];
  const located = listed.filter((l) => l.gmb.location !== null);
  const locatedRef = useRef(located);
  // first of the effects, so each one after reads this render's
  useEffect(() => {
    locatedRef.current = located;
  });
  const markers = useRef(new Map<number, { marker: google.maps.marker.AdvancedMarkerElement; dot: HTMLSpanElement; listed: Listed }>());
  const shown = located.map((l) => `${l.gmb.id}:${l.gmb.enabled}:${l.gmb.location?.lat},${l.gmb.location?.lng}`).join(" ");

  useEffect(() => () => setOnMap({ inView: null, hovered: null, peeked: null }), []);

  useEffect(() => {
    if (map.status !== "ok") return;
    const m = map.map;
    const update = () => {
      const bounds = m.getBounds();
      if (bounds === undefined) return; // before the first render: the next idle tells
      setOnMap({ inView: new Set(locatedRef.current.filter((l) => l.gmb.location && bounds.contains(l.gmb.location)).map((l) => l.gmb.id)) });
    };
    const idle = m.addListener("idle", update);
    update();
    return () => idle.remove();
  }, [map, shown]);

  useEffect(() => {
    if (map.status !== "ok" || fitted || gmails.status !== "ok") return;
    fitted = true;
    const points = locatedRef.current.flatMap((l) => (l.gmb.location ? [l.gmb.location] : []));
    if (points.length === 1) {
      map.map.setCenter(points[0]!);
      map.map.setZoom(14);
    } else if (points.length > 1) {
      const bounds = new google.maps.LatLngBounds();
      for (const p of points) bounds.extend(p);
      map.map.fitBounds(bounds, 48);
    }
  }, [map, gmails.status]);

  useEffect(() => {
    if (map.status !== "ok") return;
    let live = true;
    const made = markers.current;
    void importLibrary("marker").then(({ AdvancedMarkerElement }) => {
      if (!live) return;
      for (const l of locatedRef.current) {
        const dot = document.createElement("span");
        dot.addEventListener("mouseenter", () => setOnMap({ hovered: l.gmb.id }));
        dot.addEventListener("mouseleave", () => setOnMap({ hovered: null }));
        const marker = new AdvancedMarkerElement({ map: map.map, position: l.gmb.location, title: l.gmb.name ?? l.gmb.place_id, content: dot, gmpClickable: true });
        marker.addEventListener("gmp-click", () => setOnMap({ peeked: l.gmb.id }));
        made.set(l.gmb.id, { marker, dot, listed: l });
      }
      styleDots(made, null, null);
    });
    return () => {
      live = false;
      for (const { marker } of made.values()) marker.map = null;
      made.clear();
    };
  }, [map, shown]);

  useEffect(() => styleDots(markers.current, hovered, peeked), [hovered, peeked, shown]);

  const peekedListed = listed.find((l) => l.gmb.id === peeked);
  useEffect(() => {
    const at = peekedListed?.gmb.location;
    if (map.status === "ok" && at) map.map.panTo(at);
  }, [map, peekedListed?.gmb.location]);

  return (
    <>
      <div ref={host} className="absolute inset-0" />
      {map.status === "error" && (
        <div className="absolute inset-x-6 top-6">
          <ErrorState failure={{ kind: "invalid", message: map.message }} />
        </div>
      )}
      {peekedListed && <Peek listed={peekedListed} onClose={() => setOnMap({ peeked: null })} />}
    </>
  );
}

function styleDots(made: Map<number, { dot: HTMLSpanElement; listed: Listed; marker: google.maps.marker.AdvancedMarkerElement }>, hovered: number | null, peeked: number | null) {
  for (const [id, { dot, listed, marker }] of made) {
    const lit = id === hovered || id === peeked;
    dot.className = cn(
      "block size-3.5 rounded-full border-2 border-background shadow-md transition-transform",
      listed.gmb.enabled ? "bg-positive" : "bg-ink-soft",
      lit && "scale-150",
      id === peeked && "ring-2 ring-ink",
    );
    marker.zIndex = lit ? 1 : 0;
  }
}
