import { http, ignoreBody } from "@/shared/api";

import { type Candidate, type Gmail, type SitesPlace, candidatesParser, gmailsParser, sitesPlacesParser } from "../model/gmb";

export async function fetchGmails(): Promise<Gmail[]> {
  return (await http.get("/api/v1/gmails", gmailsParser)).gmails;
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

export async function addGmb(gmailId: number, placeId: string): Promise<void> {
  await http.send("POST", `/api/v1/gmails/${gmailId}/gmbs`, { place_id: placeId }, ignoreBody);
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
