import { ApiError, http, ignoreBody } from "@/shared/api";

import { type Candidate, type Gmail, type Gmb, type LatLng, type MapConfig, type SitesPlace, candidatesParser, gmailsParser, gmbParser, mapConfigParser, sitesPlacesParser } from "../model/gmb";

export async function fetchGmails(): Promise<Gmail[]> {
  return (await http.get("/api/v1/gmails", gmailsParser)).gmails;
}

/** `null`: the conductor has no browser key, so there is no map. */
export async function fetchMapConfig(): Promise<MapConfig | null> {
  try {
    return await http.get("/api/v1/map", mapConfigParser);
  } catch (e) {
    if (e instanceof ApiError && e.failure.kind === "not_found") return null;
    throw e;
  }
}

export async function addGmail(gmail: string): Promise<void> {
  await http.send("POST", "/api/v1/gmails", { gmail }, ignoreBody);
}

export async function removeGmail(id: number): Promise<void> {
  await http.send("DELETE", `/api/v1/gmails/${id}`, undefined, ignoreBody);
}

export async function resolvePlace(place: string): Promise<Candidate[]> {
  return (await http.get("/api/v1/gmbs/resolve", candidatesParser, { place })).candidates;
}

export function addGmb(gmailId: number, placeId: string): Promise<Gmb> {
  return http.send("POST", `/api/v1/gmails/${gmailId}/gmbs`, { place_id: placeId }, gmbParser);
}

/** Refused when Google has the place's point: only one it has none of is placed by hand. */
export async function placeGmb(id: number, at: LatLng): Promise<void> {
  await http.send("PUT", `/api/v1/gmbs/${id}/location`, { lat: at.lat, lng: at.lng }, ignoreBody);
}

export async function removeGmb(id: number): Promise<void> {
  await http.send("DELETE", `/api/v1/gmbs/${id}`, undefined, ignoreBody);
}

/** All of `ids` or none: one not the caller's refuses the lot. */
export async function switchGmbs(ids: number[], enabled: boolean): Promise<void> {
  await http.send("PUT", "/api/v1/gmbs/enabled", { gmbs: ids, enabled }, ignoreBody);
}

export async function fetchSitesPlaces(): Promise<SitesPlace[]> {
  return (await http.get("/api/sites/places", sitesPlacesParser)).places;
}

/** sites' place answer is not read: the page re-reads its tags. */
export async function linkPlace(brand: string, location: string, gmbId: number | null): Promise<void> {
  await http.send("PUT", `/api/sites/places/${encodeURIComponent(brand)}/${encodeURIComponent(location)}/gmb`, { gmb_id: gmbId }, ignoreBody);
}
