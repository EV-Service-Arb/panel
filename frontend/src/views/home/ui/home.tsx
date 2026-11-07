"use client";

import { Skeleton, cn } from "@evinvest/uikit";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";

import { type LatLng, type Listed, type MapConfig, type Tags, fetchMapConfig, issueOf, narrowed, setOnMap, useGmailScope, useGmails, useGmbTags, useOnMap } from "@/entities/gmb";
import { PlacingBar } from "@/features/place-gmb";
import { takeReturnPage } from "@/shared/api";
import { useT } from "@/shared/i18n";
import { useResource } from "@/shared/lib/use-resource";
import { pin, useTheMap } from "@/shared/map";
import { EmptyState } from "@/shared/ui/empty-state";
import { ErrorState } from "@/shared/ui/error-state";

import { MapLayers } from "./map-layers";
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
  const { hovered, peeked, placing } = useOnMap();
  const { tags } = useGmbTags();
  const { host, map } = useTheMap(config.key);
  const listed = gmails.status === "ok" ? narrowed(gmails.data, scope, "") : [];
  const located = listed.filter((l) => l.gmb.location !== null);
  const locatedRef = useRef(located);
  // first of the effects, so each one after reads this render's
  useEffect(() => {
    locatedRef.current = located;
  });
  const markers = useRef(new Map<number, { remove: () => void; dot: HTMLSpanElement; listed: Listed }>());
  const shown = located.map((l) => `${l.gmb.id}:${l.gmb.enabled}:${l.gmb.location?.lat},${l.gmb.location?.lng},${l.gmb.location?.placed}`).join(" ");
  const styling = useRef({ hovered, peeked, tags });
  useEffect(() => {
    styling.current = { hovered, peeked, tags };
  });

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
    const made = markers.current;
    for (const l of locatedRef.current) {
      const dot = document.createElement("span");
      dot.title = l.gmb.name ?? l.gmb.place_id;
      dot.addEventListener("mouseenter", () => setOnMap({ hovered: l.gmb.id }));
      dot.addEventListener("mouseleave", () => setOnMap({ hovered: null }));
      dot.addEventListener("click", () => setOnMap({ peeked: l.gmb.id }));
      made.set(l.gmb.id, { remove: pin(map.map, dot, l.gmb.location!).remove, dot, listed: l });
    }
    styleDots(made, styling.current);
    return () => {
      for (const { remove } of made.values()) remove();
      made.clear();
    };
  }, [map, shown]);

  useEffect(() => styleDots(markers.current, { hovered, peeked, tags }), [hovered, peeked, tags, shown]);

  const placed = placing === null || gmails.status !== "ok" ? undefined : gmails.data.flatMap((g) => g.gmbs).find((b) => b.id === placing);
  const placedRef = useRef(placed);
  useEffect(() => {
    placedRef.current = placed;
  });
  const [moved, setMoved] = useState<{ gmb: number; at: LatLng } | null>(null);
  const placedAt = placed !== undefined && moved?.gmb === placed.id ? moved.at : null;
  const placingPin = useRef<ReturnType<typeof pin> | null>(null);
  useEffect(() => {
    const gmb = placedRef.current;
    if (map.status !== "ok" || gmb === undefined) return;
    const move = (at: LatLng) => setMoved({ gmb: gmb.id, at });
    const click = map.map.addListener("click", (e: google.maps.MapMouseEvent) => e.latLng && move(e.latLng.toJSON()));
    const centre = map.map.getCenter();
    if (centre === undefined) throw new Error("the map has no centre while shown");
    const at = gmb.location ?? centre.toJSON();
    const dot = document.createElement("span");
    dot.title = gmb.name ?? gmb.place_id;
    dot.className = "block size-5 rounded-full border-2 border-background bg-accent-error shadow-md ring-2 ring-ink";
    dot.style.zIndex = "3";
    placingPin.current = pin(map.map, dot, at, move);
    move(at);
    return () => {
      click.remove();
      placingPin.current?.remove();
      placingPin.current = null;
    };
  }, [map, placed?.id]);
  useEffect(() => {
    if (placingPin.current && moved) placingPin.current.move(moved.at);
  }, [moved]);

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
      {peekedListed && placed === undefined && <Peek listed={peekedListed} onClose={() => setOnMap({ peeked: null })} />}
      {map.status === "ok" && <MapLayers map={map.map} />}
      {map.status === "ok" && placed !== undefined && placedAt !== null && <PlacingBar gmb={placed} map={map.map} at={placedAt} onMove={(at) => setMoved({ gmb: placed.id, at })} />}
    </>
  );
}

/** A pin is red while its GMB needs the operator, faded while it is off, and hollow where its point was placed by hand. */
function styleDots(
  made: Map<number, { dot: HTMLSpanElement; listed: Listed }>,
  { hovered, peeked, tags }: { hovered: number | null; peeked: number | null; tags: Tags },
) {
  for (const [id, { dot, listed }] of made) {
    const lit = id === hovered || id === peeked;
    const issue = issueOf(listed.gmb, tags);
    const tone = issue === "off" ? "ink-soft" : issue !== null ? "negative" : "positive";
    const hollow = listed.gmb.location?.placed === true;
    dot.className = cn(
      "block size-3.5 rounded-full border-2 shadow-md transition-transform",
      hollow ? "bg-background" : "border-background",
      hollow && { negative: "border-accent-error", positive: "border-positive", "ink-soft": "border-ink-soft" }[tone],
      !hollow && { negative: "bg-accent-error", positive: "bg-positive", "ink-soft": "bg-ink-soft" }[tone],
      issue === "off" && !lit && "opacity-50",
      lit && "scale-150",
      id === peeked && "ring-2 ring-ink",
    );
    dot.style.zIndex = lit ? "2" : issue === "off" ? "0" : "1";
  }
}
