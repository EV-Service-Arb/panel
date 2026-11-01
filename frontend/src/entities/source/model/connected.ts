import { type Infer, type Parser, arrayOf, nullable, object, oneOf, recordOf, str } from "@/shared/lib/parse";

/** A site key's origin as `panel_core::ids::SiteOrigin` takes it; the backend has the last word. */
export function isOrigin(s: string): boolean {
  try {
    const u = new URL(s);
    return u.origin === s && (u.protocol === "https:" || ["localhost", "127.0.0.1"].includes(u.hostname));
  } catch {
    return false;
  }
}

const sourceStateParser = object({
  state: oneOf(["unset", "live", "empty", "withdrawn", "down"] as const),
  detail: nullable(str),
});
export type SourceState = Infer<typeof sourceStateParser>;

const reportParser = object({
  brand: str,
  kitstart: str,
  checked_at: str,
  sources: object({ places: recordOf(sourceStateParser), pricing: sourceStateParser, experiments: sourceStateParser }),
});

export type Reported =
  | { state: "no_origin" }
  | { state: "reported"; report: Infer<typeof reportParser> }
  | { state: "failed"; detail: string };

const reportedParser: Parser<Reported> = (v, path) => {
  const { state } = object({ state: oneOf(["no_origin", "reported", "failed"] as const) })(v, path);
  switch (state) {
    case "no_origin":
      return { state };
    case "reported":
      return { state, report: object({ report: reportParser })(v, path).report };
    case "failed":
      return { state, detail: object({ detail: str })(v, path).detail };
  }
};

type Read<T> = { state: "read"; value: T } | { state: "failed"; detail: string };

function readOf<T>(p: Parser<T>): Parser<Read<T>> {
  return (v, path) => {
    const { state } = object({ state: oneOf(["read", "failed"] as const) })(v, path);
    return state === "read" ? { state, value: object({ value: p })(v, path).value } : { state, detail: object({ detail: str })(v, path).detail };
  };
}

const ENDPOINTS = ["locations", "pricing", "experiments"] as const;

export const connectedSitesParser = object({
  sites: arrayOf(object({ key_id: str, brands: arrayOf(str), origin: nullable(str), report: reportedParser })),
  received: readOf(recordOf(str)),
  internal_reads: readOf(
    object({
      since: str,
      reads: recordOf(object({ locations: nullable(str), pricing: nullable(str), experiments: nullable(str) })),
    }),
  ),
});

/** `null`: not since sites began counting; `undefined`: sites could not be asked. */
type Seen = string | null | undefined;

export interface ConnectedSite {
  keyId: string;
  brands: string[];
  origin: string | null;
  report: Reported;
  lastIngest: Seen;
  /** The latest read of the key's brands from sites' `/api/internal`, and each one read. */
  lastRead: Seen;
  reads: { brand: string; endpoint: (typeof ENDPOINTS)[number]; at: string }[];
}

export interface ConnectedSites {
  sites: ConnectedSite[];
  /** What sites answered instead, once for the page. */
  sitesFailures: string[];
}

export function joinConnectedSites(raw: Infer<typeof connectedSitesParser>): ConnectedSites {
  const { received, internal_reads: internal } = raw;
  const sites = raw.sites.map((s) => {
    const reads =
      internal.state === "read"
        ? s.brands.flatMap((brand) => ENDPOINTS.flatMap((endpoint) => {
            const at = internal.value.reads[brand]?.[endpoint];
            return at ? [{ brand, endpoint, at }] : [];
          }))
        : [];
    const latest = reads.reduce<string | null>((l, r) => (l === null || Date.parse(r.at) > Date.parse(l) ? r.at : l), null);
    return {
      keyId: s.key_id,
      brands: s.brands,
      origin: s.origin,
      report: s.report,
      lastIngest: received.state === "read" ? (received.value[s.key_id] ?? null) : undefined,
      lastRead: internal.state === "read" ? latest : undefined,
      reads,
    };
  });
  const sitesFailures = [received, internal].flatMap((r) => (r.state === "failed" ? [r.detail] : []));
  return { sites, sitesFailures };
}
