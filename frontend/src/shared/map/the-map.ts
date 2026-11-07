"use client";

// One Google map per page session, kept while its page is away and re-inserted when it is back:
// every `google.maps.Map` built is a billed map load. The script comes straight from Google.
// Raster, without a map id: only such a map takes `styles`, which is what hides Google's own places.

import { importLibrary, setOptions } from "@googlemaps/js-api-loader";
import { type RefObject, useEffect, useRef, useState, useSyncExternalStore } from "react";

const PLACES_KEY = "sa:map-places";
const PLACES_EVENT = "sa:map-places-chosen";
const PLACES_HIDDEN: google.maps.MapTypeStyle[] = [
  { featureType: "poi", stylers: [{ visibility: "off" }] },
  { featureType: "transit", elementType: "labels.icon", stylers: [{ visibility: "off" }] },
];

const placesShown = () => localStorage.getItem(PLACES_KEY) === "shown";
const placesStyles = () => (placesShown() ? [] : PLACES_HIDDEN);

let made: { key: string; element: HTMLDivElement; map: Promise<google.maps.Map> } | null = null;

function theMap(key: string, scheme: string): { element: HTMLDivElement; map: Promise<google.maps.Map> } {
  if (made !== null) {
    if (made.key !== key) throw new Error("the map's key changed within a page session");
    return made;
  }
  if (scheme !== "dark" && scheme !== "light") throw new Error(`--scheme is ${JSON.stringify(scheme)}, not dark or light`);
  setOptions({ key, v: "weekly" });
  const element = document.createElement("div");
  element.style.width = "100%";
  element.style.height = "100%";
  const map = Promise.all([importLibrary("maps"), importLibrary("core")]).then(
    ([{ Map, RenderingType }, { ColorScheme }]) =>
      new Map(element, {
        renderingType: RenderingType.RASTER,
        colorScheme: scheme === "dark" ? ColorScheme.DARK : ColorScheme.LIGHT,
        styles: placesStyles(),
        // the world, until the page fits what it shows
        center: { lat: 0, lng: 0 },
        zoom: 2,
        disableDefaultUI: true,
        zoomControl: true,
        clickableIcons: false,
      }),
  );
  made = { key, element, map };
  return made;
}

export type TheMap = { status: "loading" } | { status: "ok"; map: google.maps.Map } | { status: "error"; message: string };

/** The page session's map, inside `host`; its polarity is the host's `--scheme`. */
export function useTheMap(key: string): { host: RefObject<HTMLDivElement | null>; map: TheMap } {
  const host = useRef<HTMLDivElement>(null);
  const [map, setMap] = useState<TheMap>({ status: "loading" });
  useEffect(() => {
    const box = host.current;
    if (box === null) throw new Error("useTheMap: its host is not rendered");
    const { element, map } = theMap(key, getComputedStyle(box).getPropertyValue("--scheme").trim());
    box.appendChild(element);
    let live = true;
    map.then(
      (m) => live && setMap({ status: "ok", map: m }),
      (e: unknown) => live && setMap({ status: "error", message: e instanceof Error ? e.message : String(e) }),
    );
    return () => {
      live = false;
      element.remove();
    };
  }, [key]);
  return { host, map };
}

const subscribePlaces = (onChange: () => void) => {
  for (const e of ["storage", PLACES_EVENT]) window.addEventListener(e, onChange);
  return () => {
    for (const e of ["storage", PLACES_EVENT]) window.removeEventListener(e, onChange);
  };
};

/** Whether the map shows Google's own places (shops, sights, stops); hidden unless chosen, kept in this browser. */
export function usePlacesShown(): boolean {
  return useSyncExternalStore(subscribePlaces, placesShown, () => false);
}

export function showPlaces(shown: boolean): void {
  if (shown) localStorage.setItem(PLACES_KEY, "shown");
  else localStorage.removeItem(PLACES_KEY);
  window.dispatchEvent(new Event(PLACES_EVENT));
  void made?.map.then((m) => m.setOptions({ styles: placesStyles() }));
}

/** `element` centred on a point of `map` until removed, its clicks the map's no more; dragged where `onDrag` is given. */
export function pin(map: google.maps.Map, element: HTMLElement, at: google.maps.LatLngLiteral, onDrag?: (to: google.maps.LatLngLiteral) => void): { move: (to: google.maps.LatLngLiteral) => void; remove: () => void } {
  element.style.position = "absolute";
  element.style.transform = "translate(-50%, -50%)";
  google.maps.OverlayView.preventMapHitsAndGesturesFrom(element);
  const overlay = new (class extends google.maps.OverlayView {
    override onAdd() {
      this.getPanes()!.overlayMouseTarget.appendChild(element);
    }
    override draw() {
      const p = this.getProjection().fromLatLngToDivPixel(at);
      if (p === null) throw new Error("a pin's point has no pixel while drawn");
      element.style.left = `${p.x}px`;
      element.style.top = `${p.y}px`;
    }
    override onRemove() {
      element.remove();
    }
  })();
  // the map draws it once added; before that a move only changes where
  const redraw = () => element.isConnected && overlay.draw();
  if (onDrag) {
    element.style.cursor = "grab";
    element.addEventListener("pointerdown", (e) => {
      element.setPointerCapture(e.pointerId);
      const box = map.getDiv().getBoundingClientRect();
      const follow = (m: PointerEvent) => {
        const to = overlay.getProjection().fromContainerPixelToLatLng(new google.maps.Point(m.clientX - box.left, m.clientY - box.top));
        if (to === null) throw new Error("the pointer's pixel has no point while dragging");
        at = to.toJSON();
        redraw();
      };
      element.addEventListener("pointermove", follow);
      element.addEventListener("pointerup", () => {
        element.removeEventListener("pointermove", follow);
        onDrag(at);
      }, { once: true });
    });
  }
  overlay.setMap(map);
  return {
    move: (to) => {
      at = to;
      redraw();
    },
    remove: () => overlay.setMap(null),
  };
}
