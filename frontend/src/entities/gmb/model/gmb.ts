import { type Infer, type Parser, arrayOf, bool, nullable, num, object, str } from "@/shared/lib/parse";

import type { Candidates, Gmails, Gmb, MapConfig, Place } from "./generated";

export type { Gmail, Gmb, MapConfig, Place as Candidate } from "./generated";

export type LatLng = NonNullable<Gmb["location"]>;

const gmbParser: Parser<Gmb> = object({
  id: num,
  place_id: str,
  name: nullable(str),
  address: nullable(str),
  gbp_account: nullable(str),
  gbp_location: nullable(str),
  created_at: str,
  enabled: bool,
  location: nullable(object({ lat: num, lng: num })),
});

export const mapConfigParser: Parser<MapConfig> = object({ key: str, map_id: str });

export const gmailsParser: Parser<Gmails> = object({ gmails: arrayOf(object({ id: num, gmail: str, created_at: str, gmbs: arrayOf(gmbParser) })) });

const candidateParser: Parser<Place> = object({ place_id: str, name: nullable(str), address: nullable(str) });
export const candidatesParser: Parser<Candidates> = object({ candidates: arrayOf(candidateParser) });

/** A JSON object keyed by GMB id, each value checked. */
function byGmb<T>(p: Parser<T>): Parser<Map<number, T>> {
  return (v, path) => {
    if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${path}: expected an object of GMB ids`);
    return new Map(
      Object.entries(v).map(([id, value]) => {
        const n = Number(id);
        if (!Number.isSafeInteger(n)) throw new Error(`${path}: ${id} is not a GMB id`);
        return [n, p(value, `${path}.${id}`)];
      }),
    );
  };
}

const archiveTagParser = object({ tracked: bool, rating: nullable(num), reviews: nullable(num) });
export const archiveParser = object({ gmbs: byGmb(archiveTagParser), places_tracked: num });
export type Archive = Infer<typeof archiveParser>;

const siteTagParser = object({ brand: str, location: str });
export type SiteTag = Infer<typeof siteTagParser>;
export const sitesParser = byGmb(siteTagParser);

const sitesPlaceParser = object({ brand: str, location: str, withdrawn: bool });
export type SitesPlace = Infer<typeof sitesPlaceParser>;
export const sitesPlacesParser = object({ places: arrayOf(sitesPlaceParser) });
