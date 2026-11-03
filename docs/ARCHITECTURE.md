# Architecture

The conductor is Service-Arb's host on `sa.evinvest.ltd`: who a person is (sessions through
concierge), what they hold across the services (the gmails and GMBs they manage, their tokens),
who a machine is (the source keys), the shell the browser loads, and the forward to the services
behind the origin — sites_panel (the sites business: leads, places, pricing, experiments,
bookings, invoices), review_archive and playbook. Each service trusts only the assertion the
conductor signs for a request; none of them sees a session, and none calls the conductor: it
reads them. Live updates and alerts are its hubs, fed by what the services tell it; tokens are
charged from what review_archive tells it.

```text
browser ─session──▶ conductor ─x-sa-assertion (User)──────────────▶ sites /api/v1, review_archive, playbook
landings/bots ─HMAC▶ conductor ─x-sa-assertion (Source) + raw body ▶ sites /api/ingest/v1
conductor ◀── pulls (Conductor) ── sites /api/conductor/{changes, notifications, sources/received,
                                                          internal-reads}
conductor ◀── pulls (Conductor) ── review_archive /api/conductor/{charges, notifications,
                                                                   captures/<sha>.png}
conductor ─── pushes (Conductor) ─▶ review_archive /api/conductor/gmbs   GMBs, balances, the cursor
conductor ── GET <origin>/.well-known/sa-site, public :443 ──────▶ each site key's landing
landings ──────────────────────── direct, in-cluster ────────────▶ sites /api/internal
```

A cargo workspace; dependencies point inwards only.

```text
crates/panel_core/       no I/O: brand ids, the HMAC of a signed request and its window, the
                         conductor's own alert messages, the buttons' signed data, retry and
                         pacing, tokens' renewal and a charge's split
crates/panel/            the engine: `Panel` (sources and their authentication, sessions, access
                         requests, the hubs' state), `seal` (XChaCha20-Poly1305 at rest), `store`
                         (SQLite, migrations/ applied on open, writes BEGIN IMMEDIATE), `alerts`
                         (the hub: access, destinations, rules, the feeds fanned out; the
                         Directory and Feeds ports), `telegram` (the method: links, chats, outbox,
                         delivery, buttons; the Bot port), `live` (the in-process bus), `gmbs`,
                         `tokens` (the ledger, review_archive's charges, the snapshot), `import`
                         (review_archive's, once)
crates/sa_alerts/        no I/O: the alert rules, and the notes a service publishes to the
                         conductor and their check; sites and review_archive take it too
crates/sa_auth/          the `sa` permissions (concierge_iam derives), the aliases `sa:operator` /
                         `sa:admin`, and the assertion: a User, a Source or the Conductor, for one
                         request of one service
crates/sa_places/        a Google place from what a person pastes (an id, a Maps URL, a Places
                         API search); review_archive takes it too
crates/sa_ui/            Dioxus components the services' front ends share
crates/panel_contracts/  concierge's gRPC contract (tonic, from contracts/proto)
crates/panel_server/     the `panel` binary: CLI and HTTP, thin over `Panel`
  src/signin.rs          /auth/login, /auth/callback, /auth/logout; the /api/v1 gate
  src/concierge.rs       concierge over gRPC: ExchangeCode, RefreshClientToken, GetMe,
                         PublishCatalog; its development stand-in (PANEL_DEV_SIGN_IN)
  src/cookies.rs         __Host- cookies, the double-submit CSRF check
  src/http.rs            the router; the /api/v1 route table and the section each sits under
  src/api.rs             GET /me, /sources
  src/connected.rs       GET /sources/sites: each site key's landing report beside sites' view
  src/access.rs          /api/v1/access/requests
  src/ingest.rs          the signed requests: authenticated here, forwarded to sites
  src/forward.rs         the services behind the origin; `Services`, the one client and signing
                         key every call to them goes through
  src/grafana.rs         Grafana: its process, /grafana/, the viewers' tokens and the loopback
                         listener they work on, the services' dashboards
  src/live.rs            GET /api/v1/live, and the follower of sites' change feed
  src/gmbs.rs            /api/v1/gmails, /api/v1/gmbs
  src/tokens.rs          /api/v1/tokens, /api/v1/admin/tokens; review_archive followed and told
  src/alerts.rs          /api/v1/alerts; the services' feeds as the hub reads them; the hub's
                         work in `serve`
  src/telegram.rs        the Bot API, its long poll
  src/web.rs             the front end's static export (PANEL_WEB_DIR), behind every other route
  src/settings.rs        the environment (ev_lib `settings!`)
crates/panel_gen/        `nix run .#gen`: the front end's mirror of Rust types (ev_lib `ts_gen`),
                         into frontend/src/entities/{session,source,gmb,tokens,alerts}/model/generated.ts,
                         committed;
                         the pre-commit hook re-runs it
contracts/proto/concierge/v1/  concierge's auth and directory protos, vendored at the commit in
                               REV (`sync.sh` refreshes them)
```

## Signing in

The conductor is a relying party of concierge, client id `sa`, with its own origin and its own
cookies; the browser never holds a concierge token.

```text
GET /auth/login      ?return_to=<path> (optional; else 400), ?prompt=select_account (optional;
                     anything else 400): state + PKCE verifier + return_to, sealed into
                     sa_prelogin (10 min)
                     → 302 <concierge>/api/auth/authorize?client_id=sa&redirect_uri=…&state&code_challenge
                       [&prompt=select_account: Google's chooser even with a live evinvest.ltd session]
GET /auth/callback   state = the cookie's (constant time), and not redeemed before
                     (consumed_states), else 400 and the code is never presented;
                     ExchangeCode(code, redirect_uri, verifier, client secret); the browser's
                     previous session, if any, closed → a session row (tokens sealed under
                     PANEL_DATA_KEY, keyed by the hash of a random id) → sa_session (HttpOnly)
                     + sa_csrf → 303 return_to, else /
/api/v1/*            not GET: x-sa-csrf must equal sa_csrf; the session (access token rotated
                     when within 30 s of expiry, see below); GetMe (cached ≤ 60 s per session,
                     asked afresh for POST/DELETE /sources, the Telegram link and a chat added,
                     one retry when concierge does not answer) → the caller's permissions, else 401 (cookies
                     cleared) / 503; then the route's section, else 403
POST /auth/logout    CSRF; every session of the user is closed
```

- **Permissions, no admission.** Any active concierge account signs in. `GetMe` answers the
  concrete `sa` permissions the user holds (no alias, no wildcard; anything outside `sa` is a
  failed answer). Every `/api/v1` route sits under one section — Admin
  (`sa:admin:sources:manage`), Tokens (`sa:admin:tokens:grant`) — or none (`/me`,
  `/access/requests*`, `/alerts*`, the caller's own gmails, GMBs and tokens). Each fresh
  `GetMe` upserts the user (`users`: id, email, name), whom tokens and gmails belong to.
  `http::api_routes` is the table the router is built from and the tests walk.
- **The catalog.** `serve` publishes `Catalog::collect("sa", PANEL_BUILD_EPOCH)` — the whole `sa`
  namespace, every service's permissions — with `PublishCatalog` before it serves (the image
  sets `PANEL_BUILD_EPOCH` to the commit's unix seconds, so an older replica never supersedes a
  newer one); concierge refusing it fails the boot. Not under dev sign-in. Its `sa:everyone`
  alias is held by every member of the `sa` scope with no grant.
- **`return_to`** is a path on this origin: it starts with `/`, the next character is not `/`,
  it has no `\`, only visible ASCII, ≤ 512 bytes. Anything else is the 400 sign-in page.
- **Revocation.** concierge refusing a refresh or `GetMe` closes the session here; concierge
  unreachable is a 503 and keeps it. A permission revoked at concierge is seen within the 60 s
  of the cache (at once by every write: see Forward). Signing out of evinvest.ltd revokes the
  concierge token family: the session ends at its next rotation.
- **Rotation, a refresh token presented once.** No pool connection is held while concierge is
  asked. In one process, a single flight per session. Across processes, a lease on the row
  (`sessions.rotating_until`, 15 s): only its holder asks, the others poll the row for up to
  6 s, then answer 503; a lease whose holder died lapses. The rotated pair is written only if
  the row still has the pair the rotation started from.
- **Bounded.** `/auth/*`: at most 8 at once (else 503), 10 s each. `/api/v1`: 32, 15 s. The pool
  gives a connection within 3 s. Every answer carries `X-Content-Type-Options: nosniff`; the
  sign-in's HTML pages a CSP of `default-src 'self'; frame-ancestors 'none'; base-uri 'none';
  form-action 'self'`.
- **Cookies** are `__Host-` and `Secure` when `PANEL_PUBLIC_ORIGIN` is https, bare over plain
  http (development only), all `SameSite=Lax`.
- **Dev sign-in** (`PANEL_DEV_SIGN_IN=sa:admin|sa:operator|<permission>,…|none`, docs/LOCAL.md):
  `Concierge::dev` answers for `PANEL_DEV_SIGN_IN_EMAIL`'s user (the database's of that email, else one named by it) holding that set; the pre-login, the state, the
  session and the gate run unchanged. Refused at boot (exit 78) in any profile but development,
  beside any concierge variable, and unless `PANEL_PUBLIC_ORIGIN` is loopback http.
- `serve` needs a sign-in: concierge's four variables (all or none), or dev sign-in.

## Forward

The services live under the conductor's origin; `forward.rs` streams each prefix to its
service, both ways (hyper-util, no buffering), one table for all of it — the front end's
reserved paths (`web.rs`) are derived from it.

```text
/api/sites/*                     → sites /api/v1/*              session + User; CSRF on writes
/api/hooks/booking/*             → sites (same path)            open: the providers' webhooks
/sites/mfe/*                     → sites /mfe/*                 open: the sites page's bundle
/api/review_archive/*            → review_archive /*            session + User; CSRF on writes
/review_archive/mfe/*            → review_archive /mfe/*        open: the dashboard's bundle
/playbook_mcp/authorize          → playbook (same path)         session + User; no conductor CSRF
/playbook_mcp/*, /.well-known/oauth-{authorization-server,protected-resource}/playbook_mcp
                                 → playbook (same path)         open: OAuth and bearer clients
```

- **The assertion** (`sa_auth`, header `x-sa-assertion`) is signed for one request: Ed25519
  (`PANEL_ASSERTION_KEY`), `aud` the service, the method and the upstream path (without its
  query), alive 60 s, and a principal: a **User** (`sub`, email, `email_verified`, name, and
  their `sa:<service>:*` permissions only), a **Source** (a key the conductor authenticated:
  `key_id`, kind, brands, and the SHA-256 of the body its MAC covered), or the **Conductor**
  itself (its hubs). The services verify it with the public half (`PANEL_ASSERTION_KEYS`,
  several while a key rotates) and trust nothing else.
- **Hygiene:** the browser's `Cookie`, any inbound `x-sa-assertion` and `x-sa-csrf`, and
  hop-by-hop headers (`Upgrade` included: nothing is upgraded through the forward) never go up;
  `Set-Cookie` never comes down. Everything else passes (`Idempotency-Key`, `X-Member`, a
  client's `Authorization`).
- **Gates:** a gated read takes the cached `GetMe`, a gated write asks concierge afresh. A write
  under `/api/sites` and `/api/review_archive` needs the conductor's CSRF header. playbook's
  consent form at `/playbook_mcp/authorize` posts without it: the `SameSite=Lax` session and
  playbook's own single-use nonce guard it; a signed-out `GET` of that page goes to
  `/auth/login?return_to=` and comes back.
- **A path stays under its upstream prefix:** a `.` or `..` segment, `%2e`, `%2f`, `%5c` and `\`
  decoded, is a 400.
- **The outline.** Every signed-in page sits beside it: the open service's own views (the sites:
  `<mfe-sites-nav>`, mounted on every page and shown on theirs; the review archive: its places,
  Telegram and settings), then the caller's gmails, each opening to its GMBs, with their switches.
  `/` is the GMBs' own page.
- **The pages.** `/sites` mounts `<mfe-sites-panel api="/api/sites" base="/sites" csrf-cookie>`
  from `/sites/mfe/`; `/review_archive` mounts
  `<mfe-review-archive-dashboard grafana="/grafana/">` from `/review_archive/mfe/`. Any `/sites/<view>` or
  `/review_archive/<view>` without a dot in its last segment is that page, for the element to
  route, and a file below a view (`/sites/<view>/index.txt`, `…/__next.*.txt`) is the page's
  own, so Next's client-side navigation to a view stays soft. The bundles run on this origin,
  under `script-src 'self'`: their builds are trusted as the conductor's own code is.
- **The elements route by the location.** Next moves it with `pushState`, which fires nothing:
  after each of its moves the shell dispatches a stateless `popstate`, as back and forward do
  (Next's own handler ignores it). An element is created once per tag, attributes and language
  and re-inserted after: a Dioxus one (ev_lib's `mfe`) cannot be torn down, so it keeps running
  while its page is not shown, and holds its last route while the location is not under its base.
- **The platform's elements are not forwarded.** The account chip, the sign-in element and the
  wallet load straight from `https://evinvest.ltd`, admitted by the CSP's directories (Deploy
  requirements). `/cabinet` stays reserved: a JSON 404, never the front end's page.
- Without `PANEL_ASSERTION_KEY`, `PANEL_REVIEW_ARCHIVE_URL`, `PANEL_PLAYBOOK_URL` and
  `PANEL_SITES_URL` (all four, or none), nothing is forwarded, nothing ingested, the hubs hear
  nothing of sites, and review_archive is neither followed nor told.

## Grafana

One Grafana for every service, at `/grafana/` on this origin: an element steers its frame through
`history` and checks the origin of its messages, which a subdomain would break. `serve` runs it
(`PANEL_GRAFANA_SERVER` and `PANEL_GRAFANA_DIR`, or neither: no Grafana) on a unix socket in its
dir, its root URL `<PANEL_PUBLIC_ORIGIN>/grafana/`.

```text
browser ── /grafana/* ────────────▶ conductor (session; a POST is a read) ──socket──▶ Grafana
browser ── /grafana/api/ds/query ─▶ conductor: a viewer token into each query whose datasource
                                    uid is a service; dead once Grafana answers
Grafana (Infinity) ── GET 127.0.0.1:<any>/api/<service>/… + token ──▶ the forward, as that viewer
conductor ── GET /dashboards (Conductor), every 60 s, conditional ──▶ each service of
             forward::DASHBOARDS ──▶ <dir>/dashboards/<service>/<uid>.json ──▶ Grafana
```

- **Datasources are derived:** one Infinity datasource per service's API (`/api/<service>`), its
  uid the service's name, its url that API on the loopback listener and nothing beyond it
  allowed; written, with the dashboards' provider, before Grafana starts.
- **Dashboards are the services':** coupled to a service's API, they ship with its release. A
  service that has them is in `forward::DASHBOARDS`, and its `GET /dashboards` answers a JSON
  array of them, each with its `uid`, and an ETag. A pull that fails — no answer, a 404, a uid
  another service serves or Grafana refuses — is reported, and what was served before stays.
- Grafana's dashboards query by POST, so every method under `/grafana/` takes the cached `GetMe`
  and no CSRF.
- Cookies, assertions, `x-sa-csrf` and the viewer header never reach Grafana, its `Set-Cookie`
  never comes down, nothing is upgraded. A Grafana that fails is reported and `/grafana/`
  answers 502; the conductor goes on.

## Ingest

The landings, the bots and the other sources sign what they send with a key the conductor
holds; the conductor authenticates, sites authorizes.

```text
POST /api/ingest/v1/events        at most 32 at once (else 503), 30 s in all (else 408)
  │ headers present, |now − x-sa-timestamp| ≤ 5 min, x-sa-key-id a slug     else 401, body unread
  │ body read: ≤ 4 MiB within 10 s                                          else 413 / 408
  │ x-sa-key-id → sources (not revoked) → secret, unsealed
  │ HMAC over "sa-ingest/v1.<x-sa-timestamp>.<body>"                        else 401
  ▼
sites POST /api/ingest/v1/events, the body as it came, Source{key_id, kind, brands, body digest}
  → its answer as it came: 207 a verdict per event, 409 + Retry-After, 400 …
GET /api/ingest/v1/leads/by-ref/{brand}/{ref}   the same, the MAC over "GET <path?query>" in
  place of a body → sites, the same path and query, Source over the empty body
```

What a key may write — its kind's types, its brands, a bot's lookup — is sites' to judge, per
event. An unknown key and a bad signature answer the same 401, so key ids cannot be probed for;
only a timestamp outside the window is told apart.

**Keys** (`/api/v1/sources`, `panel source add|list|set-origin|revoke`): one kind and a set of
brands each, the secret shown once. Kinds `panel` and `booking` are written by sites itself: no
key is issued for them. A `site` key carries its landing's origin (`https://<domain>[:port]`,
nothing after it; plain http on `localhost` / `127.0.0.1` for a landing on this machine): given
when the key is issued, or once with `set-origin` for a key issued before origins.

## The conductor's API, `/api/v1`

JSON; timestamps RFC 3339; errors `{"error": "…"}` with 400 / 403 / 404 / 409 / 503.

```text
GET    /me                                  {user_id, email, preferred_name, permissions,
                                            dev_sign_in}: every sa permission, every service's
GET    /sources              Admin          {sources: [{key_id, kind, brands, created_at, revoked_at,
                                            origin}]}
POST   /sources              Admin, fresh   {key_id, kind, brands, origin} → 201 {key_id, secret}
                                            (shown once), 409 if taken; kind panel or booking, a
                                            site without an origin or another kind with one → 400
GET    /sources/sites        Admin          the Connected sites table, below
DELETE /sources/{key_id}     Admin, fresh   204, 404
POST   /access/requests                     {need} → 201 {need, requested_at}, made now; 200 the one
                                            standing (made within 24 h: nobody told again);
                                            400 a need neither a permission nor an alias of the
                                            sa catalog; 409 a need held already
GET    /access/requests/mine                {requests: [{need, requested_at}]}; a need is held
                                            when every permission it stands for is, and both
                                            routes drop the caller's requests held
GET    /alerts                              {enabled, bot, rules, destinations: [{id, kind, title,
                                            topic_id, gmail_id, dead, rules: {rule: bool}}]} (the
                                            rules open to the caller)
POST   /alerts/telegram/link fresh          → 201 {url: "https://t.me/<bot>?start=<token>"};
                                            503 without a bot
POST   /alerts/telegram/chats fresh         {chat, gmail_id} → 201 as GET; 400 refused (nothing
                                            stored), 409 a destination already
PUT    /alerts/destinations/{id}/rules      {rules: {new_lead: false, …}} → 200 as GET; 400 for a
                                            rule not open to them
PUT    /alerts/destinations/{id}/gmail      {gmail_id} → 200 as GET; 404 not theirs
POST   /alerts/destinations/{id}/test       204; 400 with what Telegram said
DELETE /alerts/destinations/{id}            204, 404
GET    /gmails                              {gmails: [{id, gmail, created_at, gmbs: [{id, place_id,
                                            name, address, gbp_account, gbp_location, created_at,
                                            enabled}]}]} the caller's, removed ones left out
POST   /gmails                              {gmail} → 201 the gmail; 409 held already
DELETE /gmails/{id}                         204; its GMBs count as removed
GET    /gmbs/resolve?place=                 {candidates: [{place_id, name, address}]} (sa_places)
POST   /gmails/{id}/gmbs                    {place_id} → 201 the GMB; 409 under that gmail already
DELETE /gmbs/{id}                           204
PUT    /gmbs/enabled                        {gmbs: [id], enabled} → 204: every one switched, or none
                                            (404 one not the caller's)
GET    /tokens                              {balance, daily, cap, kickbacks}: renewed, the
                                            kick-backs not claimed
GET    /tokens/ledger?before=&limit=        {entries, next}: newest first, limit 1–200
GET    /tokens/usage                        {days: [{day, charges, tokens}]}: 30 UTC days
POST   /tokens/claim                        → 200 as GET /tokens
GET    /admin/tokens         Tokens         {users: [{user_id, email, name, balance}]}
POST   /admin/tokens/{user}  Tokens, fresh  {set: n, note?} | {add: n, note?} | {grant: n, note}
                                            → 200 {balance}; a grant is owed until claimed
```

An id in a path that is not the caller's is a 404. Before the import (below) the gmail and GMB
writes and every tokens route answer 503.
**Connected sites** (`GET /sources/sites`): for every active site key at once, its landing's own
report and sites' view of it, all asked together (joined, nothing spawned):

```text
{sites: [{key_id, brands, origin, report}], received, internal_reads}
  report          {state: "reported", report: <the landing's /.well-known/sa-site body>}
                  | {state: "failed", detail} | {state: "no_origin"}
                  GET <origin>/.well-known/sa-site: no redirect followed, 5 s, ≤ 64 KiB, 200 and
                  the report's shape, its brand one of the key's — else failed
  received        sites GET /api/conductor/sources/received  {state: "read", value: {key_id: at}}
  internal_reads  sites GET /api/conductor/internal-reads    {state: "read", value: {since, reads:
                  {brand: {locations, pricing, experiments}}}}; either {state: "failed", detail}
                  (5 s; or no forward)
```

The page `/access?need=&continue=` (outside the shell) is where a service sends a signed-in user
lacking `need`: it asks, then reads `/me` every 30 s and goes on to `continue` (a path, the
`return_to` rule) once held.

## Live updates, `/api/v1/live`

A screen reads through the API and is told, over one WebSocket, when what it read may have
changed. Nothing is sent but the hint.

```text
GET /api/v1/live    Origin = PANEL_PUBLIC_ORIGIN exactly                     else 403
                    the session, as the /api/v1 gate (cached GetMe)          else 401 / 503
                    ≤ 5 sockets per user, ≤ 200 in all                       else 429
                    → 101; any signed-in user: what they are told is judged per message
server → client     {"type":"hello","at","user_id"}                          at once
                    {"type":"changed","topic","brand_id"?,"id"?,"at"}        a change they may read
                    {"type":"resync"}                                        read everything again
                    WS Ping every 25 s; no Pong within 60 s → dropped
closes              4401 the session ended   1001 the server is stopping
```

- **What sites changed** comes from its feed, `GET /api/conductor/changes` (NDJSON, a Conductor
  assertion): one follower per process puts each `changed` line on the bus with the permission
  it names — told to whoever holds it — and each `resync` to everyone. sites sends `resync`
  first on every connection and a `ping` every 25 s; a feed that ends, fails, or says nothing
  for 60 s is followed again after 1 s, doubling to 30 s, so what was missed meanwhile is read
  again.
- **The conductor's own topics:** `sources` (a key minted or revoked; holders of
  `sa:admin:sources:manage`) and `alerts` (the user's destinations added, removed, found dead
  or revived; their rules; that user only).
- **The shell relays.** The front end holds the one socket and re-dispatches on `window`, as
  `CustomEvent`s: `sa:live` (each server message as is, and a `{type:"resync"}` after the shell
  reconnects), `sa:live-status` (`{status}`; answered to an element's
  `sa:live-status-request`). The sites elements dispatch `sa:title-count` (`{count}`, the nav
  only) and `sa:unauthenticated` (the shell opens its sign-in, returning to the current page).
- **The bus is in-process** (`tokio::sync::broadcast`, one pod), 1024 messages per subscriber; a
  socket further behind skips the backlog and is sent `resync`. A frame that does not go out
  within 10 s drops the socket; its write buffer is capped at 256 KiB, a client frame at 4 KiB.
  Handshakes: 16 at once, 15 s each.
- **The session is asked again** every 60 s, and at once when a session ends: a sign-out (every
  socket of the user), a sign-in replacing the browser's session, concierge refusing a rotation
  or `GetMe`. Concierge unreachable keeps the socket.

## Tokens and GMBs

A person's gmails and the GMBs (Google places) each manages are the conductor's; the services
store their ids. A GMB is on or off by its owner's switch (`gmbs_off`), which the services inherit
for their own work: off, review_archive walks none of its tracks. Tokens are a ledger: a balance is its rows' sum, renewed a day's worth when
read (`TOKENS_DAILY` up to `TOKENS_CAP`; a first balance `TOKENS_START_CAP_MULTIPLE` caps). What
spends them — today review_archive's walks — charges after the fact.

```text
follow (10 s)   GET review_archive /api/conductor/charges?after=<cursor>&limit=500 (Conductor)
                → one write transaction, taken only if the cursor is still `after`: each charge's
                payers (subs) renewed, its cost split over them within their balances, a
                `charge` row each (service, key `run:<id>`, label), what a failed walk owes back
                as kick-backs, the cursor moved; an unknown sub fails the page, the cursor stays
push (a change, a page, 60 s)
                PUT review_archive /api/conductor/gmbs (Conductor): every GMB ever (removed ones
                marked, each on or off), every user's balance renewed, and the cursor they were read at, from
                one transaction
```

- **The archive's allowance** is that snapshot's balance less its own charges past the cursor,
  so a walk is neither counted twice nor missed, and walks go on while the conductor is away.
  Two services spending one balance could overlap by at most a walk each; holds come with a
  second spender.
- **A 404 on `/api/conductor/*`** is an archive not cut over yet: told once at info, asked again
  every 60 s.
- **The import opens it.** `panel import review-archive <file>` (review_archive's
  `scripts/export_members.sql`) writes its people, gmails, GMBs (ids kept), ledger and pending
  kick-backs in one transaction, refused unless the tables are empty, and creates the cursor.
  Until then tokens and GMBs are closed and `serve` neither follows nor pushes: a balance read
  writes a renewal, which would make the import refuse. A fresh install imports an empty
  document.
- **A place's site** is sites': its `places.gmb_id`, linked from the outline through the forward;
  sites down or refusing leaves the GMBs without the link.

## Alerts

One hub: rules, their audiences, destinations and the fan-out are the conductor's; the services
only publish notes (`sa_alerts`), and Telegram is one method of reaching a destination
(`TELEGRAM_BOT_TOKEN`; it needs the forward). Without a bot, nothing is sent.

```text
rule               feed            audience                                         default
new_lead           sites           sa:sites:work:leads:edit                         on
contact_overdue    sites           sa:sites:work:leads:edit                         on
booked             sites           sa:sites:work:leads:edit                         on
payment_received   sites           sa:admin:sources:manage                          off
source_silent      conductor       sa:admin:sources:manage                          off
access_requested   conductor       every permission of sa:admin                     on
review_gone        review_archive  the owners of the GMBs the note names            on
review_new, review_changed, review_reappeared, scan_failed   (the same)             off
```

```text
feed (2 s)        GET <service>/api/conductor/notifications?after=<cursor>&limit=500 (Conductor), each
                  service under its own cursor → one write transaction, taken only from where the
                  cursor stands: each note checked against its feed (its rule's, its GMBs and photo
                  as its rule says, its buttons callable), claimed (rule, event) and queued for its
                  audience, the cursor moved to the page's `next`; a note that fails fails the page,
                  the cursor stays (loud, never skipped); 404: a service not cut over, said once,
                  asked again every 60 s
own (2 s)         sources silent ≥ 24 h (sites' last receipt per key, else the key's creation; once
                  per full day of it), access requests (once a day per user and need)
audience          a destination: the rule on there, its user's access confirmed within the hour;
                  a permission rule: their permissions open it; a GMB rule: they own a GMB the note
                  names, the GMB on, it and its gmail not removed, the destination's gmail theirs if
                  it is scoped to one
delivery (0.5 s)  lead messages noted > 1 h ago dead ("stale"); > 20 of one rule waiting for one
                  destination folded into one summary without PII; claim in one write transaction
                  (nothing while the outbox is paused): ≤ 25 tries started in any second, one per
                  private chat per second, one per channel or group per 3 s, none to a chat with a
                  send in flight; leased 60 s → the user's access checked again → sendMessage, or
                  sendPhoto with the photo fetched from the note's service (Conductor) → sent |
                  retry | dead
```

- **Destinations.** A user's own private chat (one), and channels, groups and forum topics they
  add. Each has its rules (a rule without a row is at its default) and may be scoped to one of
  the user's gmails for the GMB rules.
- **Private chat.** `POST /api/v1/alerts/telegram/link` (GetMe asked afresh; 256 random bits,
  base64url; SHA-256 stored with the caller's permissions, 10 min) → `t.me/<bot>?start=<token>`;
  `/start <token>`, in a private chat, not forwarded: the token redeemed once, the chat the user's
  private destination (a chat linked before is let go, the rules kept); a chat that is another
  account's is refused ("/stop first"), the token kept. `/stop` removes it.
- **Channels, groups, topics.** A paste (`@channel`, `-100<id>`, `<group>/<topic>`) is taken
  only after `getChat` sees it, `getChatMember` says the user's own Telegram account (their
  private chat's id; linked first) administers it, and a test went out there: a refusal stores
  nothing. They get the note's non-PII text, never buttons, never the conductor's access
  requests. One destination per chat and topic.
- **A note is its service's rendering**: its text and Telegram entities, a PII text sent only to
  private chats of holders of `sa:sites:work:pii:see`, the user who is not told (`skip_user`), its
  buttons (`{id, label, pressed, call: {method, path under /api/v1/, body}, leaves}`, sites'),
  the GMBs it names, a photo path (`review_gone`'s capture, ≤ 1024 UTF-16 of caption, no
  buttons). Queued texts are sealed under `PANEL_DATA_KEY` per chat and topic and dropped once
  sent or dead; no parse mode, link previews off.
- **Updates by long polling.** One process polls, under a lease in `telegram_poller` (60 s,
  renewed every poll); the offset is stored there after each update, and every update is
  idempotent, so one handled twice across a takeover does nothing twice.
- **Retries.** 429 pauses the whole outbox for `retry_after` and costs the message no try; 5xx,
  timeouts and a photo not fetched back off from 10 s, doubling, to 15 min; 10 tries, then dead.
  403 and 400 "chat not found" / "user is deactivated" mark every destination in the chat dead
  until a send there succeeds again (a link, a test). Any other 4xx is dead at once; so is a
  queued message that no longer opens.
- **Quiet.** The bot answers commands in private chats only; its unsolicited replies go at most
  once per chat per 10 min.
- **Who gets a message: access concierge confirmed within the hour.** The hub keeps per user the
  set last confirmed and when: the `/api/v1` gate records every `GetMe` answer, and every 15 min
  the hub asks again for a user with a destination reached, not confirmed since, through their
  newest session used within 7 days. The set is checked again when a message is sent. A grant
  revoked at concierge, or a session concierge refuses to rotate (or whose `GetMe` it refuses
  twice running), stops messages at the next check and drops what was queued; no session used
  for a week, or concierge unreachable, stops them after an hour.
- **Buttons.** The callback data is `<button id>.<outbox id>.<HMAC-SHA256, 80 bits>` under a key
  derived from `PANEL_DATA_KEY`, bound to the chat; a message sent with one-letter buttons
  (`t`, `n`) presses as `take`, `no_answer`. The message must be that chat's, the chat the
  private chat of the message's user, and the user's access is asked of concierge at the press —
  with no live session to ask with, the answer is "open the panel". The button's call goes to
  sites as that user (a User assertion, the sites slice) with `Idempotency-Key: tg/<outbox
  id>/<button id>`, so a press repeated, or its update redelivered, records nothing twice. 201:
  the message gains "{pressed}: {name}" and keeps the buttons the press `leaves`; 200, 400, 409
  "already recorded"; 404 "not found"; 403 "no access"; anything else "try again".
- Language of the conductor's own texts: `TELEGRAM_LOCALE` (`ru` default, `en`).

## Invariants

- **A service trusts the conductor's assertion and nothing else**, and one assertion opens one
  request: its audience, its method, its path, 60 s; a User's permissions only of that service;
  a Source's over the body its MAC covered. The conductor never forwards a cookie, nor an
  assertion it did not sign.
- **The conductor authenticates, sites authorizes.** A signed request reaches sites only with a
  verified MAC over its timestamp and body (the 5-minute window cannot be dodged by re-stamping);
  what the key may write is never judged here.
- **Grafana holds no identity.** It signs no one in: its fetches reach a service as a viewer only
  through a token the conductor wrote into that viewer's `/grafana/api/ds/query`, on the loopback
  listener, on GETs, for that query's life. The public listener never takes a token.
- **A source is only ever revoked.** `sources` refuses DELETE and any UPDATE but of `revoked_at`
  and a site's `origin` set once from NULL (only a `site` has one); a key id is never reused.
- **Secrets never sit in the clear.** Sources' HMAC secrets, sessions' tokens and queued Telegram
  texts are sealed (XChaCha20-Poly1305, what they belong to as associated data) under
  `PANEL_DATA_KEY`, whose fingerprint is stored beside each.
- **Alerts are the conductor's.** A service publishes notes and never reaches a person itself;
  who is told, where and how is decided here.
- **A note is in the outbox before the cursor passes it**: the page and the cursor move in one
  transaction. **So is a charge in the ledger**, and a page is taken only from where the cursor
  stands.
- **The ledger is append-only, and nothing a service stores an id of is deleted.**
  `token_ledger` refuses UPDATE and DELETE; gmails and GMBs only get `removed_at`, kick-backs
  only `claimed_at`, each once.
- **One file, migrated on open.** `PANEL_DB_PATH` names it (`/data/panel.db` in the image); every
  command creates it if missing and applies the migrations it lacks first (`panel migrate` does
  only that). A migration newer than the build is let be. Tables are STRICT, foreign keys
  enforced, timestamps INTEGER microseconds since the epoch, UUIDs 16-byte blobs.
- **Secrets come from the environment only** (`PANEL_DATA_KEY`, `SENTRY_DSN`,
  `RP_CLIENT_SECRET_SA`, `PANEL_ASSERTION_KEY`, `TELEGRAM_BOT_TOKEN`, `GOOGLE_MAPS_KEY`), through
  `ev_lib::settings`; with `APP_ENV=production` the database, the data key, the four sign-in
  variables, the forward's four and `TOKENS_START_CAP_MULTIPLE` are required at boot
  (`panel --print-required-vars` lists them).

## Deploy requirements

- **One image, one origin.** The image carries the binary and the front end's static export,
  and sets `PANEL_WEB_DIR` to it; `serve` answers `/api`, `/auth` and `/health` first, then the
  forward's prefixes, then the files (a directory by its `index.html`, anything else `404.html`
  with a 404). `/cabinet` is held, and `/grafana` without Grafana: JSON 404s. `/_next/static/*` is `immutable`
  for a year, everything else `no-cache`; every page carries a CSP of `default-src 'self'`,
  scripts from this origin and inline only by hash (`web::Files::new` hashes every inline
  `<script>` of every page at boot), `'unsafe-inline'` for styles, `'wasm-unsafe-eval'` (the
  review_archive dashboard), `frame-ancestors 'none'`, `Referrer-Policy: same-origin`, and
  Trusted Types report-only.
- **Grafana's memory.** ~340 MB at its peak, ~185 of it its binary's pages, and its Infinity plugin
  server ~80 MB at a board's load, ~110 under a sustained one: the pod's limit has room for both
  above the conductor. Its dir is `/data/grafana`, unreplicated: provisioned again on every start.
- **The platform's MFE bundles (`PANEL_CSP_PLATFORM_MFE`).** `0` (default) leaves the CSP as
  above; `1` adds to `script-src` the directories `https://evinvest.ltd/api/auth/mfe/` and
  `https://evinvest.ltd/wallet/mfe/` and `https://challenges.cloudflare.com` (the sign-in
  element's Turnstile), to `style-src` the wallet's directory, to `connect-src` the origin
  `https://evinvest.ltd`, and a `frame-src` of `'self'` and Turnstile's origin. Directories, not
  the origin: only the elements the platform publishes for hosts run in the session.
- **The contract** (`nix eval .#containers.<system>.panel.contract`) names the port (the flake's `port`),
  `/health`, `APP_ENV=production`, the variables required, secret and optional,
  `PANEL_DB_PATH`, the volume (`/data`) and the SQLite file on it to replicate, what the ingress
  must not publish or must rate-limit, and the egress. `checks.contract-env` fails the flake
  when its required variables and the binary's `--print-required-vars` disagree.
- **What it provides and consumes, by contract name.** `provides.ingest` is
  `/api/ingest/v1/events`; `consumes` names the peers behind `PANEL_SITES_URL` (`sites_panel`),
  `PANEL_REVIEW_ARCHIVE_URL` (`review_archive`) and `PANEL_PLAYBOOK_URL` (`playbook_web`), whose
  URLs and egress devops derives. `checks.provided-paths` fails the flake when `provides` and the
  binary's `--print-provided-paths` (the constants its router mounts) disagree.
- **The landings, outbound on :443.** The Connected sites page asks each site key's origin; the
  origins are data, so the egress is public :443, not a host list.
- **Ingest stays inside the cluster.** Its sources reach it by service DNS; the IngressRoute that
  publishes `sa.evinvest.ltd` must not route `/api/ingest`, and the NetworkPolicy lets in only
  the pods that send.
- **sites_panel within reach, and only through here.** The conductor calls `PANEL_SITES_URL` for
  everything it forwards and for its hubs; sites has no public route of its own (the landings
  reach its `/api/internal` in-cluster).
- **concierge within reach.** The pods call concierge's gRPC (`CONCIERGE_GRPC_ADDR`) for every
  sign-in, rotation and `GetMe`. concierge must register client `sa` with redirect URI
  `<PANEL_PUBLIC_ORIGIN>/auth/callback` exactly and hold the hash of `RP_CLIENT_SECRET_SA`.
- **Places, outbound.** With `GOOGLE_MAPS_KEY` the pods need egress to
  `places.googleapis.com:443` (a pasted URL without a place id, a GMB's name and address).
- **Telegram, outbound only.** With `TELEGRAM_BOT_TOKEN` the pods need egress to
  `api.telegram.org:443`; no webhook is set on the bot (`getUpdates` refuses to run while one is).
- **`/api/v1/live` through the public IngressRoute, as a WebSocket.** Nothing in front may buffer
  the response or strip `Upgrade` / `Connection` / `Origin`, and an idle timeout on the way must
  be longer than the 25 s ping.
- **Rate-limit `/auth` and `/api/hooks` per client IP at Traefik**; the conductor bounds how many
  run at once, not who starts them.
- **One pod, a volume, litestream.** One writer: one replica, rolled out with `Recreate`. No
  init container: `serve` migrates on start.
