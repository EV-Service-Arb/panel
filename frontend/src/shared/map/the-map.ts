"use client";

// One Google map per page session, kept while its page is away and re-inserted when it is back:
// every `google.maps.Map` built is a billed map load. The script comes straight from Google.

import { importLibrary, setOptions } from "@googlemaps/js-api-loader";
import { type RefObject, useEffect, useRef, useState } from "react";

let made: { key: string; mapId: string; element: HTMLDivElement; map: Promise<google.maps.Map> } | null = null;

function theMap(key: string, mapId: string, scheme: string): { element: HTMLDivElement; map: Promise<google.maps.Map> } {
  if (made !== null) {
    if (made.key !== key || made.mapId !== mapId) throw new Error("the map's key or id changed within a page session");
    return made;
  }
  if (scheme !== "dark" && scheme !== "light") throw new Error(`--scheme is ${JSON.stringify(scheme)}, not dark or light`);
  setOptions({ key, v: "weekly" });
  const element = document.createElement("div");
  element.style.width = "100%";
  element.style.height = "100%";
  const map = Promise.all([importLibrary("maps"), importLibrary("core")]).then(
    ([{ Map }, { ColorScheme }]) =>
      new Map(element, {
        mapId,
        colorScheme: scheme === "dark" ? ColorScheme.DARK : ColorScheme.LIGHT,
        // the world, until the page fits what it shows
        center: { lat: 0, lng: 0 },
        zoom: 2,
        disableDefaultUI: true,
        zoomControl: true,
        clickableIcons: false,
      }),
  );
  made = { key, mapId, element, map };
  return made;
}

export type TheMap = { status: "loading" } | { status: "ok"; map: google.maps.Map } | { status: "error"; message: string };

/** The page session's map, inside `host`; its polarity is the host's `--scheme`. */
export function useTheMap(key: string, mapId: string): { host: RefObject<HTMLDivElement | null>; map: TheMap } {
  const host = useRef<HTMLDivElement>(null);
  const [map, setMap] = useState<TheMap>({ status: "loading" });
  useEffect(() => {
    const box = host.current;
    if (box === null) throw new Error("useTheMap: its host is not rendered");
    const { element, map } = theMap(key, mapId, getComputedStyle(box).getPropertyValue("--scheme").trim());
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
  }, [key, mapId]);
  return { host, map };
}
