/**
 * A stand-in for the conductor's backend, for `next dev` without Rust or concierge: `/api/v1`
 * (who you are, sources, access requests, gmails, tokens), `/auth/*`, review_archive's and sites' GMB tags, and
 * the live socket. Same shapes, same gate (401 without a session, CSRF on writes). The sites'
 * API and bundle are sites_panel's: run it for `/sites`.
 *
 *   npm run dev:stub                  # :3121, signed in as an operator
 *   STUB_ROLE=admin npm run dev:stub  # as an admin
 *   STUB_ME=403 npm run dev:stub      # the gate refusing: 401 | 403 | 503
 *   STUB_LIVE=off npm run dev:stub     # no /api/v1/live socket (4401 | 4403: close at once)
 *   STUB_LIVE_EVERY=5 npm run dev:stub # live activity every 5 s rather than every 20–40 s
 *
 * Live: every write above is announced on the socket, and the sites are busy too — a `leads`
 * change every 20–40 s, relayed to the elements as it would be from sites_panel's feed.
 *
 * then `npm run dev` in another shell. Data is made up and says so ("stub").
 */
import { randomUUID } from "node:crypto";
import { type IncomingMessage, type ServerResponse, createServer } from "node:http";
import type { Duplex } from "node:stream";

import { ALIASES } from "../src/entities/session/model/generated.ts";
import { changed, every, liveUpgrade } from "./stub-live.ts";

const PORT = Number(process.env.STUB_PORT ?? 3121);
const ROLE = process.env.STUB_ROLE === "admin" ? "admin" : "operator";
const ME_FAILURE = process.env.STUB_ME;
const CSRF = "stub-csrf";
const LIVE_EVERY = process.env.STUB_LIVE_EVERY ? Number(process.env.STUB_LIVE_EVERY) : null;
const USER_ID = "00000000-0000-7000-8000-000000000001";

type Json = Record<string, unknown>;
const now = () => new Date();
const iso = (d: Date) => d.toISOString();
const minsAgo = (m: number) => iso(new Date(now().getTime() - m * 60_000));

const sources: Json[] = [
  { key_id: "aquafix-site", kind: "site", brands: ["aquafix"], created_at: minsAgo(60 * 24 * 10), revoked_at: null, origin: "https://aquafix.top" },
];

/** What `GET /sources/sites` answers: the stub's site keys, each reporting live. */
function connectedSites(): Json {
  const live = { state: "live" };
  const sites = sources
    .filter((s) => s.kind === "site" && s.revoked_at === null)
    .map((s) => ({
      key_id: s.key_id,
      brands: s.brands,
      origin: s.origin,
      report: s.origin
        ? { state: "reported", report: { brand: (s.brands as string[])[0], kitstart: "stub", checked_at: iso(now()), sources: { places: { stub: live }, pricing: live, experiments: { state: "empty" } } } }
        : { state: "no_origin" },
    }));
  return {
    sites,
    received: { state: "read", value: Object.fromEntries(sites.map((s) => [s.key_id, minsAgo(5)])) },
    internal_reads: { state: "read", value: { since: minsAgo(60), reads: { aquafix: { locations: minsAgo(2), pricing: minsAgo(3), experiments: null } } } },
  };
}

function send(res: ServerResponse, status: number, body?: unknown, headers: Record<string, string | string[]> = {}): void {
  res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store", ...headers });
  res.end(body === undefined ? undefined : JSON.stringify(body));
}

async function readJson(req: IncomingMessage): Promise<Json> {
  const chunks: Buffer[] = [];
  for await (const c of req) chunks.push(c as Buffer);
  const text = Buffer.concat(chunks).toString();
  return text ? (JSON.parse(text) as Json) : {};
}

function signedIn(req: IncomingMessage): boolean {
  return /(?:^|;\s*)sa_session=/.test(req.headers.cookie ?? "");
}

const gmails: { id: number; gmail: string; created_at: string; gmbs: Json[] }[] = [
  {
    id: 1,
    gmail: "owner@gmail.com",
    created_at: minsAgo(60 * 24 * 30),
    gmbs: [
      { id: 1, place_id: "ChIJstubstubstub00001", name: "Plombier Paris 11 (stub)", address: "12 rue Oberkampf, Paris", gbp_account: null, gbp_location: null, created_at: minsAgo(60 * 24 * 30) },
      { id: 2, place_id: "ChIJstubstubstub00002", name: "Serrurier Lyon 3 (stub)", address: "4 cours Gambetta, Lyon", gbp_account: null, gbp_location: null, created_at: minsAgo(60 * 24 * 20) },
    ],
  },
];
const tokens = { balance: 75, daily: 15, cap: 500, kickbacks: [{ id: 1, at: minsAgo(90), amount: 12, service: "review_archive", key: "run:42", label: "Plombier Paris 11 (stub)", by: null, note: null }] };
const ledger = Array.from({ length: 8 }, (_, i) => ({ id: 8 - i, at: minsAgo(60 * 24 * i), delta: i % 2 ? -12 : 15, kind: i % 2 ? "charge" : "accrual", service: i % 2 ? "review_archive" : null, key: i % 2 ? `run:${40 - i}` : null, label: i % 2 ? "Plombier Paris 11 (stub)" : null, by: null, note: null }));
const usageDays = Array.from({ length: 30 }, (_, i) => ({ day: iso(new Date(now().getTime() - (29 - i) * 86_400_000)).slice(0, 10), charges: i % 3, tokens: (i % 3) * 12 }));

/** Never granted: the stub's role is fixed, so a request waits for good. */
const accessRequests: { need: string; requested_at: string }[] = [];

async function api(req: IncomingMessage, res: ServerResponse, url: URL): Promise<void> {
  const path = url.pathname.replace(/^\/api\/v1/, "");
  const write = req.method !== "GET";
  if (write && req.headers["x-sa-csrf"] !== CSRF) return send(res, 403, { error: "csrf" });
  if (ME_FAILURE === "403") return send(res, 403, { error: "no access to the panel" });
  if (ME_FAILURE === "503") return send(res, 503, { error: "sign-in is unavailable, try again" });
  if (ME_FAILURE === "401" || !signedIn(req)) return send(res, 401, { error: "sign in" }, { "set-cookie": "sa_session=; Path=/; Max-Age=0" });

  // The stub signs anyone in, as the backend's dev sign-in does: it says so.
  if (path === "/me") return send(res, 200, { user_id: USER_ID, permissions: ALIASES[`sa:${ROLE}`], email: "stub@example.test", preferred_name: `Stub ${ROLE}`, dev_sign_in: true });
  if (path === "/access/requests/mine") return send(res, 200, { requests: accessRequests });
  if (path === "/access/requests" && write) {
    const { need } = (await readJson(req)) as { need: string };
    const standing = accessRequests.find((r) => r.need === need);
    if (standing) return send(res, 200, standing);
    const made = { need, requested_at: new Date().toISOString() };
    accessRequests.push(made);
    return send(res, 201, made);
  }
  if (path === "/gmails" && req.method === "GET") return send(res, 200, { gmails });
  if (path === "/gmails" && req.method === "POST") {
    const { gmail } = (await readJson(req)) as { gmail: string };
    const made = { id: gmails.length + 1, gmail, created_at: iso(now()), gmbs: [] };
    gmails.push(made);
    return send(res, 201, made);
  }
  if (path === "/gmbs/resolve") return send(res, 200, { candidates: [{ place_id: "ChIJstubstubstub00009", name: "Électricien Lille (stub)", address: "8 rue Faidherbe, Lille" }] });
  if (path === "/tokens") return send(res, 200, tokens);
  if (path === "/tokens/ledger") return send(res, 200, { entries: ledger, next: null });
  if (path === "/tokens/usage") return send(res, 200, { days: usageDays });
  if (path === "/admin/tokens") return send(res, 200, { users: [{ user_id: USER_ID, email: "stub@example.test", name: `Stub ${ROLE}`, balance: tokens.balance }] });
  if (path.startsWith("/sources") && ROLE !== "admin") return send(res, 403, { error: "your permissions do not allow this" });
  if (path === "/sources" && req.method === "GET") return send(res, 200, { sources });
  if (path === "/sources/sites" && req.method === "GET") return send(res, 200, connectedSites());
  if (path === "/sources" && req.method === "POST") {
    const b = await readJson(req);
    if (sources.some((s) => s.key_id === b.key_id)) return send(res, 409, { error: `a source ${String(b.key_id)} exists already` });
    sources.push({ key_id: b.key_id, kind: b.kind, brands: b.brands, created_at: iso(now()), revoked_at: null, origin: b.origin });
    changed("sources");
    return send(res, 201, { key_id: b.key_id, secret: `stub-secret-${randomUUID()}` });
  }
  const revoke = path.match(/^\/sources\/([^/]+)$/);
  if (revoke && req.method === "DELETE") {
    const s = sources.find((x) => x.key_id === decodeURIComponent(revoke[1]!));
    if (!s) return send(res, 404, { error: "not found" });
    s.revoked_at = iso(now());
    changed("sources");
    return send(res, 204);
  }

  return send(res, 404, { error: "not found" });
}

// ---- the sites at work ----------------------------------------------------------------

every(LIVE_EVERY, () => changed("leads", "aquafix", `live-${randomUUID().slice(0, 8)}`));

const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (url.pathname === "/auth/login") {
    // No concierge: signing in is instant. Plain names, as the backend uses over http.
    res.writeHead(303, { location: "/", "set-cookie": ["sa_session=stub; Path=/; HttpOnly; SameSite=Lax", `sa_csrf=${CSRF}; Path=/; SameSite=Lax`] });
    return res.end();
  }
  if (url.pathname === "/auth/logout" && req.method === "POST") {
    return send(res, 204, undefined, { "set-cookie": ["sa_session=; Path=/; Max-Age=0", "sa_csrf=; Path=/; Max-Age=0"] });
  }
  if (url.pathname === "/api/review_archive/me/gmbs") {
    if (!signedIn(req)) return send(res, 401, { error: "sign in" });
    return send(res, 200, { gmbs: { "1": { tracked: true, rating: 4.6, reviews: 132 }, "2": { tracked: false, rating: null, reviews: null } }, places_tracked: 1 });
  }
  if (url.pathname === "/api/sites/gmbs") {
    if (!signedIn(req)) return send(res, 401, { error: "sign in" });
    return send(res, 200, { "1": { brand: "aquafix", location: "paris-11" } });
  }
  if (url.pathname === "/api/sites/places") {
    if (!signedIn(req)) return send(res, 401, { error: "sign in" });
    return send(res, 200, { places: [{ brand: "aquafix", location: "paris-11", withdrawn: false }, { brand: "aquafix", location: "lyon-3", withdrawn: false }] });
  }
  if (url.pathname.startsWith("/api/v1/")) {
    api(req, res, url).catch((e: unknown) => send(res, 500, { error: String(e) }));
    return;
  }
  send(res, 404, { error: "not found" });
});
server.on("upgrade", (req: IncomingMessage, socket: Duplex) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (url.pathname !== "/api/v1/live") return socket.destroy();
  liveUpgrade(req, socket, { signedIn: signedIn(req) && ME_FAILURE !== "401", userId: USER_ID });
});
server.listen(PORT, "127.0.0.1", () => console.log(`panel dev stub on http://127.0.0.1:${PORT} as ${ROLE}`));
