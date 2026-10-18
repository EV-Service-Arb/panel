# Architecture

What the panel is for, and the phases it is built in, is `SA-PANEL-SPEC.md` (§ numbers in the
code refer to it). This is where things live.

A cargo workspace of four crates; dependencies point inwards only.

```text
contracts/proto/sa/v1/events.proto   sa.funnel.v1: the Event envelope, the ingest request and
                                     answer, one properties message per type@version
crates/panel_contracts/              that proto, generated at build time: prost messages + pbjson
                                     serde (protojson); no checks beyond the JSON shape
crates/panel_core/                   no I/O: no database, network, clock or randomness
  src/ids.rs                         EventId (UUIDv7, ev_lib's Id), brand/location/lead/job ids
  src/event.rs                       SourceKind, Envelope, TypeKey (type@version), KeyGrant:
                                     which kind and brands a signing key may write
  src/fact.rs                        the registered types, typed (Fact), and what each needs of
                                     its subject
  src/lead.rs                        Stage, and fold: a lead's facts → its stage and stage times
  src/signature.rs                   the HMAC scheme of a batch and its replay window
  src/notify.rs                      Telegram: the rules and who gets each, the texts, the
                                     buttons' signed data, retry and pacing constants
  src/experiment.rs                  a brand's experiments as configuration: a declaration
                                     checked, an admin's patch, the fold to declared / override /
                                     effective, the landings' rules for an override field
  src/analytics.rs                   what PostHog is told of a lead event (name, properties
                                     without PII, the person)
  src/place.rs                       a place's live settings (kitstart's PlaceLive), checked
                                     field by field; a change and who made it
  src/pricing.rs                     a brand's price list (kitstart's PricingModel): checked,
                                     priced to the cent; tests/fixtures/pricing vendored by sha
  src/booking.rs                     providers, a place's booking config and its URL rule, a
                                     lead's booking folded; tests/fixtures/booking vendored by sha
  src/phone.rs                       kitstart's normalizePhone, ported (E.164)
crates/panel/                        the engine
  src/lib.rs                         the `Panel` facade: ingest, sources, PII, rebuild
  src/wire.rs                        protojson → the core: one event decoded and checked; the
                                     registry (type@version → properties message → Fact)
  src/seal.rs                        XChaCha20-Poly1305 at rest: PII, sources' HMAC secrets
  src/store/                         SQLite (runtime sqlx queries, embedded migrations/,
                                     applied on open; writes BEGIN IMMEDIATE)
  src/store/events.rs                the journal
  src/store/projections.rs           leads, calls, payments
  src/store/sources.rs               the signing keys
  src/store/telegram.rs              links, rules, fan-out marks, the outbox and its pacing
  src/telegram.rs                    linking, the rules' fan-out, delivery, the buttons; the
                                     Bot and Directory ports
  src/access.rs                      a user's access requests: made, standing a day, dropped once held
  src/store/access.rs                access_requests
  src/experiment.rs                  the experiments listed, changed by an admin (journaled), and
                                     what a landing is answered; the link to PostHog's funnel
  src/capture.rs                     the PostHog outbox sent in batches, retried, given up; the
                                     Capturer port
  src/place.rs                       a place's settings changed (optimistic concurrency, revert,
                                     withdraw) and what a site is answered
  src/store/places.rs                places, place_settings, the place_changes history
  src/pricing.rs                     a brand's pricing saved or removed (optimistic concurrency),
                                     the preview, and what a site is answered
  src/store/pricing.rs               pricing, pricing_changes, brand_locales
  src/booking.rs                     operators' slots, the providers' seam (PushSource,
                                     PullSource), matching, the leased pull
  src/store/bookings.rs              booking_events, bookings, booking_sync
  src/store/experiments.rs           experiments, each brand's folded from its events
  src/store/posthog.rs               posthog_outbox
  src/live.rs                        the in-process bus of what changed, published after each
                                     commit (see "Live updates")
  src/testing.rs                     (feature `testing`) throwaway SQLite files, signed batches
  migrations/                        the schema: the init (`reporting_*` views and the
                                     journal's append-only triggers included), then one file
                                     per change, each with its `down`
crates/sa_auth/                      the `sa` permissions (concierge_iam derives) and aliases
                                     `sa:operator` / `sa:admin`; the assertion the panel signs
                                     for the services behind it
crates/panel_server/                 the `panel` binary: CLI and HTTP, thin over `Panel`
  src/http.rs                        POST /api/ingest/v1/events, a bot's GET
                                     /api/ingest/v1/leads/by-ref/…, GET /health; the sign-in and
                                     /api/v1 mounted on top when signing in is configured; the
                                     /api/v1 route table and the section each sits under
  src/signin.rs                      /auth/login, /auth/callback, /auth/logout; the /api/v1 gate
  src/concierge.rs                   concierge over gRPC: ExchangeCode, RefreshClientToken, GetMe,
                                     PublishCatalog; its development stand-in (PANEL_DEV_SIGN_IN)
  src/cookies.rs                     __Host- cookies, the double-submit CSRF check
  src/api.rs                         the operator API: JSON over the engine's `operator` module
  src/access.rs                      /api/v1/access/requests: a need checked against the catalog
  src/live.rs                        GET /api/v1/live, the WebSocket that tells the screens what
                                     changed
  src/places.rs                      a place's settings: the editor's routes, and the sites'
                                     GET /api/internal/…, which has no session
  src/pricing.rs                     a brand's pricing: the editor's routes, the preview, and
                                     the sites' GET /api/internal/brands/{brand}/pricing
  src/telegram.rs                    the Bot API (reqwest), the bot's background work in
                                     `serve`, and /api/v1/telegram
  src/experiments.rs                 /api/v1/experiments and the sites' GET
                                     /api/internal/brands/{brand}/experiments
  src/capture.rs                     PostHog's capture API (reqwest) and the sender in `serve`
  src/booking.rs                     /api/v1 booking routes, POST /api/hooks/booking/…
  src/google_calendar.rs             the google_calendar pull adapter (reqwest), its schedule,
                                     the CLI's OAuth consent
  src/web.rs                         the front end's static export (PANEL_WEB_DIR), behind
                                     every other route
  src/settings.rs                    the environment (ev_lib `settings!`)
crates/panel_gen/                    `nix run .#gen`: the front end's mirror of Rust types and
                                     tables (ev_lib `ts_gen`), into frontend/**/generated.ts,
                                     committed; the pre-commit hook re-runs it
contracts/proto/concierge/v1/        concierge's auth and directory protos, vendored at the
                                     commit in REV (`sync.sh` refreshes them)
```

## Ingest → journal → projections

```text
POST /api/ingest/v1/events        at most 32 at once (else 503), 30 s in all (else 408)
  │ headers present, |now − x-sa-timestamp| ≤ 5 min, x-sa-key-id a slug     else 401, body unread
  │ body read: ≤ 4 MiB within 10 s                                          else 413 / 408
  │ x-sa-key-id → sources (not revoked) → secret, unsealed
  │ HMAC over "sa-ingest/v1.<x-sa-timestamp>.<body>"                        else 401
  │ body → {"events": [1..500]}                                             else 400
  ▼
each event on its own ─ decode (protojson) ─ envelope checks ─ key may write this kind and brand?
  │                                                                        no → rejected{reason}
  ▼
registry: type@version known?
  ├─ no  → journal, status unregistered                                   → accepted (not projected)
  ├─ yes, properties or subject wrong                                     → rejected{reason}
  └─ yes → one transaction:
             INSERT events … ON CONFLICT (id) DO NOTHING
               ├─ id taken, same content                                  → duplicate
               ├─ id taken, other content                                 → rejected
               └─ new: a call / payment row; its lead or its brand's experiments
                  recomputed; a PostHog outbox row for a lead event         → accepted
```

A bot (source kind `bot`) signs the same way, and also looks a lead up by its messenger ref:
`GET /api/ingest/v1/leads/by-ref/{brand}/{ref}`, the MAC over `GET <path?query>` in place of a
body, so a signature opens that one lookup — see
[BOT-API.md](BOT-API.md) and [Messenger leads](#messenger-leads).

## Signing in (§4)

The panel is a relying party of concierge, client id `sa`, with its own origin and its own
cookies; the browser never holds a concierge token.

```text
GET /auth/login      ?return_to=<path> (optional; else 400), ?prompt=select_account (optional;
                     anything else 400): state + PKCE verifier + return_to, sealed into
                     sa_prelogin (10 min)
                     → 302 <concierge>/api/auth/authorize?client_id=sa&redirect_uri=…&state&code_challenge
                       [&prompt=select_account: Google's chooser even with a live evinvest.ltd session;
                       the top bar's account menu, More and /account link here]
GET /auth/callback   state = the cookie's (constant time), and not redeemed before
                     (consumed_states), else 400 and the code is never presented;
                     ExchangeCode(code, redirect_uri, verifier, client secret); the browser's
                     previous session, if any, closed → a session row (tokens sealed under
                     PANEL_DATA_KEY, keyed by the hash of a random id) → sa_session (HttpOnly)
                     + sa_csrf → 303 return_to, else /
/api/v1/*            not GET: x-sa-csrf must equal sa_csrf; the session (access token rotated
                     when within 30 s of expiry, see below); GetMe (cached ≤ 60 s per session,
                     asked afresh for POST/DELETE /sources, one retry when concierge does not
                     answer) → the caller's permissions, else 401 (cookies cleared) / 503;
                     then the route's section (below), else 403
POST /auth/logout    CSRF; every session of the user is closed
```

- **Permissions, no admission.** Any active concierge account signs in. `GetMe` answers the
  concrete `sa` permissions the user holds (`UserProfile.permissions`: no alias, no
  wildcard; anything outside `sa` is a failed answer); the gate admits every signed-in user,
  and each `/api/v1` route sits under one section — Work (`sa:work:read`), Analysis
  (`sa:analysis:read`), Admin (`sa:admin:sources:manage`) — or none (`/me`, the profile's
  `/telegram*`, `/access/requests*`). `/me` also names `account_center` — concierge's `/cabinet/settings`, `null`
  under dev sign-in — so the static export never bakes in an origin. `http::api_routes` is the table the router is built from and the tests
  walk. Actions ask their own permission on top (`sa:work:leads:edit`, `sa:work:pii:see`,
  `sa:work:places:edit`, `sa:work:pricing:edit`, `sa:analysis:experiments:edit`).
- **The catalog.** `serve` publishes `Catalog::collect("sa", PANEL_BUILD_EPOCH)` with
  `PublishCatalog` before it serves (the image sets `PANEL_BUILD_EPOCH` to the commit's
  unix seconds, so an older replica never supersedes a newer one); concierge refusing it
  fails the boot. Not under dev sign-in.
- **`return_to`** is a path on this origin: it starts with `/`, the next character is not `/`,
  it has no `\`, only visible ASCII, ≤ 512 bytes. Anything else is the 400 sign-in page,
  never a silent `/`.
- **Revocation.** concierge refusing a refresh or `GetMe` closes the session here; concierge
  unreachable is a 503 and keeps it. A permission revoked at concierge is seen within the 60 s of
  the cache (at once by the source-key mutations). Signing out of evinvest.ltd revokes the
  concierge token family: the panel's session ends at its next rotation, so within the
  access token's lifetime.
- **Rotation, a refresh token presented once.** No pool connection is held while concierge
  is asked. In one process, a single flight per session: the others wait and find the fresh
  row. Across processes, a lease on the row (`sessions.rotating_until`, taken by one
  conditional UPDATE, 15 s — longer than a call to concierge): only its holder asks, the
  others poll the row for up to 6 s, then answer 503; a lease whose holder died lapses and is
  taken over. The rotated pair is written only if the row still has the pair the rotation
  started from. If the holder's answer is lost after concierge rotated, the next rotation
  presents a spent token and concierge closes the family: it fails closed.
- **Bounded.** `/auth/*`: at most 8 at once (else 503), 10 s each. `/api/v1`: 32, 15 s. The
  pool gives a connection within 3 s or the request fails. Every answer carries
  `X-Content-Type-Options: nosniff`; the sign-in's HTML pages a CSP of `default-src 'self';
  frame-ancestors 'none'; base-uri 'none'; form-action 'self'`.
- **Cookies** are `__Host-` and `Secure` when `PANEL_PUBLIC_ORIGIN` is https, bare over plain
  http (development only), all `SameSite=Lax`: the callback arrives by a top-level navigation.
- `serve` without any of the four sign-in variables answers ingest alone; some but not all
  of them is a configuration error at boot.
- **Dev sign-in** (`PANEL_DEV_SIGN_IN=sa:admin|sa:operator|<permission>,…|none`, each name
  checked against the catalog at boot, docs/LOCAL.md): `Concierge::dev` answers
  ExchangeCode, RefreshClientToken and GetMe for one made-up user holding that set, and `/auth/login`
  redirects straight to `/auth/callback?code=dev-sign-in&state=…`; the pre-login, the state,
  the session and the gate run unchanged. Refused at boot (exit 78, every command) in any
  profile but development, beside any concierge variable, and unless `PANEL_PUBLIC_ORIGIN`
  is `http://localhost[:port]` or `http://127.0.0.1[:port]` — the image is
  `APP_ENV=production`, so it cannot be on there.

## Forward

The services behind Service-Arb live under the panel's origin; `crates/panel_server/src/forward.rs`
streams each prefix to its service, both ways (hyper-util, no buffering), one table for all of
it — the front end's reserved paths (`web.rs`) are derived from it.

```text
/api/review_archive/*            → review_archive /*            session + assertion; CSRF on writes
/review_archive/mfe/*            → review_archive /mfe/*        open: the dashboard's bundle
/playbook_mcp/authorize          → playbook (same path)         session + assertion; no panel CSRF
/playbook_mcp/*, /.well-known/oauth-{authorization-server,protected-resource}/playbook_mcp
                                 → playbook (same path)         open: OAuth and bearer clients
/cabinet/mfe/<name>.{js,css}     → cabinet /cabinet/mfe/*       open, GET/HEAD: the account chip's bundle
/auth/mfe/{account-chip,sign-in}.js
                                 → concierge /auth/mfe/<hashed> open, GET/HEAD: the chip and the sign-in
                                                                element; only PANEL_CHIP_SOURCE=concierge
```

- **The cabinet** (`forward::Cabinet`, `PANEL_CABINET_URL`) is not in the table: it is no
  `sa_auth` service, gets no assertion and is mounted with or without signing in. It serves
  the account chip's files only, and is held tighter than the rows above. A name is plain
  segments of `[A-Za-z0-9._-]` (no empty one, none starting with a dot, no `%` anywhere)
  ending in `.js` or `.css` — no source maps, the cabinet's sources stay off this origin;
  anything else is a 404 that never reaches the cabinet. Only `Accept`, `If-None-Match` and
  `If-Modified-Since` go up (GET, no query); only `Content-Type`, `ETag`, `Last-Modified` and
  `Vary` come down, with `nosniff` and the panel's own `Cache-Control: public, max-age=300`.
  The body is read whole (at most 4 MiB, 10 s in all) to check that a `.js` is JavaScript and
  a `.css` CSS, and that it is the pinned bytes (below); a 304 passes, a 404 is a 404,
  anything else — a redirect, an encoded body, a pin mismatch included — is a 502. At most
  32 files at once, past that a 503.
  Unset, `/cabinet` is still reserved: a JSON 404, never the front end's page.
- **concierge's elements** (`forward::ConciergeMfe`, `PANEL_CHIP_SOURCE=concierge`) are the
  same kind of route, held by the same code (limits, headers, type check, 4 MiB / 10 s, no
  cookie either way, pin or 502), for `account-chip.js` and `sign-in.js` under `/auth/mfe/`
  (concierge's own path; publicly it sits under `/api`, which the panel has no use for).
  Concierge names its files by content hash, so the panel first reads its manifest
  `/auth/mfe/registry.json` (at most 64 KiB, 5 s) for the entry of the element
  (`concierge.account-chip`, `concierge.sign-in`) and fetches the file that names, under the
  stable name the page loads. The manifest is no trust, only a path to try: just the last
  segment of its `scriptUrl` is used, and only if it is a plain `.js` name; the pin decides.
  concierge inlines each element's stylesheet, so there is no `.css`. Pins are
  `PANEL_CONCIERGE_MFE_PINS` (`account-chip.js=<hex>,sign-in.js=<hex>`, any other name fails
  the boot), the origin is `PANEL_CONCIERGE_URL` (concierge's web port, a bare `http://`
  origin, not its gRPC). `PANEL_CHIP_SOURCE` is `cabinet` (default) or `concierge`; the cabinet's
  route stays mounted either way, and the three variables are read only under `concierge`, so
  a half-done rollout cannot fail the boot of a panel still serving the chip from the cabinet.
  Differences from the cabinet's route: nothing of the browser goes up, so concierge never
  answers 304 and the pin is checked on every byte it sends; the checked bytes of each file
  are kept in memory until restart (the pin is fixed by the environment), so concierge is
  asked once per file and concurrent first requests wait on one fetch; the panel's own `ETag`
  is the pin and it answers a matching `If-None-Match` with a 304 itself; a file is at most
  1 MiB; any failure of concierge (a 404 included) is a 502, a 404 is only for a name that
  is not served or not pinned.
  Under `concierge`, a URL without pins (or the reverse) mounts nothing and `serve` warns;
  malformed values fail the boot. *Updating:* the build of concierge's `web/` prints the pins;
  set them in the deploy together with (or before) rolling concierge, as for the cabinet.
- **The chip's files are pinned** (`PANEL_CABINET_MFE_PINS`). The bundle runs on the panel's
  origin, under the panel's session and CSP: whoever controls its bytes acts as the signed-in
  operator. The cabinet is another team's release, reached over in-cluster plain HTTP, so
  its answering is not trust enough; each file must also hash to the SHA-256 named for it:
  `account-chip.js=<hex>,account-chip.css=<hex>` (lowercase hex, `sha256sum`'s spelling).
  - *Fail closed.* A name without a pin is a 404 and the cabinet is not asked; bytes that
    hash otherwise are a 502 and nothing of them comes down. The URL without the pins (or
    the pins without the URL) mounts nothing, and `serve` warns, naming what is unset. Pins
    that do not parse fail the boot.
  - *A pin, not SRI.* `<script integrity>` would put the hash into the front end's build, so
    every cabinet release would need a panel release; a pin is deploy configuration, changed
    without a build. Checked here, it holds for every page that loads `/cabinet/mfe/*` and
    for direct fetches too, and a mismatch never reaches a browser.
  - *No compression.* A pin is of the file as built, so `Accept-Encoding` does not go up and
    an encoded answer is refused rather than unpacked; the files are a few dozen KiB, and
    the ingress may compress on the way out.
  - *Cache.* The panel sets the lifetime, not the cabinet (whose
    `stale-while-revalidate` would keep a file past the pin's change): five minutes, so a
    pin change reaches the pages that soon. A 304 passes unchecked — it carries no bytes,
    and the browser's copy came from here, checked.
  - *Updating.* A cabinet release that changes the chip changes its hashes: in `banking`,
    `npm run mfe:pins` prints the value for the release's build; set it in the deploy
    together with (or before) rolling the cabinet. Until both agree the chip is a 502 and
    the panel's pages run without it — the outage is the chip's, not the panel's.

- **Who is calling** is told by an assertion the panel signs for that one request (`sa_auth`,
  header `x-sa-assertion`): Ed25519 (`PANEL_ASSERTION_KEY`), `aud` the service, the caller's
  `sub`, email, `email_verified`, name, their `sa:<service>:*` permissions only, the
  method and the upstream path, alive 60 s. The services verify it with the public half
  (`PANEL_ASSERTION_KEYS`, several while a key rotates) and trust nothing else.
- **Hygiene:** the browser's `Cookie`, any inbound `x-sa-assertion` and `x-sa-csrf`, and
  hop-by-hop headers never go up; `Set-Cookie` never comes down — a service sets nothing on
  the panel's origin. Everything else passes (`X-Member`, a client's `Authorization`).
- **Gates:** a gated read takes the cached `GetMe`, a gated write asks concierge afresh. A
  write under `/api/review_archive` needs the panel's CSRF header. The consent form playbook
  serves at `/playbook_mcp/authorize` posts without it: the `SameSite=Lax` session and
  playbook's own single-use nonce (bound to `sub` and the pending request) guard it. A
  signed-out `GET` of that page goes to `/auth/login?return_to=` and comes back; a call
  without a session is a 401.
- **The page** `/review_archive` mounts `<mfe-review-archive-dashboard api base sign-in
  csrf-cookie>` from `/review_archive/mfe/` (`shared/mfe`), its stylesheet demoted into the
  `mfe` layer; any `/review_archive/<view>` without a dot loads the same page, for the
  dashboard to route.
- **Accepted risk:** the bundle runs on the panel's origin, unsandboxed, with the session it
  rides on — review_archive's build is trusted as the panel's own code is.
- Without `PANEL_ASSERTION_KEY`, `PANEL_REVIEW_ARCHIVE_URL` and `PANEL_PLAYBOOK_URL` (all
  three, or none: a configuration error at boot), nothing is forwarded.

## The operator API, `/api/v1`

JSON; timestamps RFC 3339, money in minor units (|amount| ≤ 10^10, currency one of EUR,
USD, GBP, AUD — checked here, not in the registry, so a rebuild never re-judges old events),
errors `{"error": "…"}` with 400 / 403 / 404 / 409 / 503. Writes need the CSRF header and
journal an event of kind `panel` (manual), so they go through the same registry and
projections as ingest.

`POST /leads`, `…/stage` and `…/payments` take an optional `Idempotency-Key` header (1–128
visible ASCII). The event id is then derived from (user, action, lead, key) — shaped as a
UUIDv7, its time field hash — and so is a new lead's id: a retry finds the first attempt in
the journal and is answered `200` with the same body, journaling nothing. Without the key,
every request is a new event (`201`).

```text
GET    /me                                        {user_id, email, preferred_name, permissions,
                                                  dev_sign_in}
GET    /leads?stage&brand&location&overdue&suspect&flow&channel&message_ref&created_from&created_to&cursor&limit
                                                  {leads: [Lead], next_cursor}; newest created
                                                  first, limit ≤ 200 (default 50); created_*
                                                  UTC days, both included; suspect=only |
                                                  exclude (absent: every lead); flow=quote |
                                                  estimate | fixed, anything else 400 (absent:
                                                  every lead; a lead that said no flow is under
                                                  none of the three); channel=form | phone_inbound
                                                  | callback | whatsapp | telegram; message_ref=
                                                  a ref as pasted (Réf. aq 7k3f: label, case,
                                                  spaces; O→0, I/L→1 in the code), the leads
                                                  carrying it
GET    /leads/counts?brand&location               {stages: {created: n, …, lost: n} (every
                                                  stage, 0 included), overdue, total}
POST   /leads                                     {brand, location, need, phone?, channel?} → 201
                                                  {brand, lead_id: "p-<uuidv7>", event_id};
                                                  channel phone_inbound (default) | whatsapp |
                                                  telegram, anything else 400
GET    /leads/{brand}/{lead}                      {lead: Lead, events: [Event]}
POST   /leads/{brand}/{lead}/stage                {stage: contacted, channel?} | {stage: quoted,
                                                  amount?, currency?} | {stage: won, job_id?} |
                                                  {stage: lost, reason, note?} | {stage: completed}
                                                  → 201 {event_id}
POST   /leads/{brand}/{lead}/messaged             {channel: whatsapp|telegram} → 201 {event_id}:
                                                  the customer wrote (lead.messaged); idempotent
POST   /leads/{brand}/{lead}/calls/attempt        → 201 {attempt_id}
POST   /leads/{brand}/{lead}/calls/{attempt}/outcome
                                                  {outcome: answered|no_answer|wrong_number|later}
POST   /leads/{brand}/{lead}/payments             {billed, commission, currency}
GET    /funnel?from&to&brand&by                   days, UTC, default the last 30, ≤ 366
                                                  {from, to, brand, min_sample, posthog_url,
                                                   stages: [{stage,
                                                   reached, of_previous, of_leads}], lost, manual,
                                                   payments: [Paid]}
                                                  by=location: {from, to, brand, min_sample,
                                                   by, locations: [{brand, location, stages, lost,
                                                   manual, payments}]}, one per brand's location
                                                   (location null for the leads naming none);
                                                   an empty window has no rows
GET    /experiments?brand                         see [Experiments](#experiments)
GET    /places                                    {places: [{brand, location, last_lead_at,
                                                  has_settings, withdrawn}]}: every location a
                                                  lead names, and every place registered
                                                  (below)
GET    /sources                     Admin         {sources: [{key_id, kind, brands, created_at,
                                                  revoked_at}]}
POST   /sources                     Admin, fresh  {key_id, kind, brands} → 201 {key_id, secret}
                                                  (shown once), 409 if taken; kind panel → 400
                                                  (the panel writes without a key)
DELETE /sources/{key_id}            Admin, fresh  204, 404
```

```text
POST   /access/requests     {need}                → 201 {need, requested_at}, made now; 200 the one
                                                  standing (made within 24 h: nobody told again);
                                                  400 a need neither a permission nor an alias of
                                                  the sa catalog; 409 a need held already
GET    /access/requests/mine                      {requests: [{need, requested_at}]}
                                                  A need is held when every permission it stands for
                                                  is; both routes drop the caller's requests held.
```

The page `/access?need=&continue=` (outside the shell) is where a service sends a signed-in
user lacking `need` — playbook's `/playbook_mcp/authorize`, for `sa:playbook:mcp:use`: it asks,
then reads `/me` every 30 s and goes on to `continue` (a path, the `return_to` rule) once held.
A ~375-byte authorize URL makes a ~478-byte `/access/?…`, under `return_to`'s 512.

```text
GET    /telegram                                  {enabled, linked, blocked, rules: {rule: bool}}
                                                  (the rules the caller's permissions open)
POST   /telegram/link                             → 201 {url: "https://t.me/<bot>?start=<token>"};
                                                  503 without a bot
DELETE /telegram/link                             204, 404
PUT    /telegram/rules      {rules: {new_lead: false, …}}
                                                  → 200 as GET; 400 for a rule they do not open
```

The funnel counts the leads that came in (were created) within the window, and `Paid` —
`{currency, billed, commission, count}` in minor units, one per currency, never converted —
sums the payments of those same leads, whenever they were paid; so a slice's money and its
`paid` step are about the same leads.

**Stages 3–4 (visits, intents) and the experiments' numbers are PostHog's**, looked at there:
the panel does not count them again (owner, 2026-10-04). It sends PostHog the leads' life after
the form instead ([PostHog](#posthog)), so the funnel from a visit to a payment is one funnel
there.
`posthog_url` opens it (`PosthogProject::funnel_url`): `location_page_view` → `sa_lead_created` →
`sa_lead_contacted` → `sa_job_won` → `sa_payment_received` over the same days, within 90 days of
the visit, filtered by `brand_id`, or broken down by it without `brand`. Null without
`POSTHOG_PROJECT_ID`.

`Lead` is the projection row (`stage`, the time of each stage, `manual`, `lost_reason`,
`suspect` — null, `"rate_limited"` or `"too_fast"`, see [Suspect leads](#suspect-leads) —,
`flow`, `quoted_cents`, `pricing_valid_from`, `estimate_inputs` — see [Flows and
prices](#flows-and-prices) —, …)
plus `sla` while it waits for its first contact — `{waiting_since, waiting_seconds,
overdue}`, overdue after 30 minutes — and `pii` (the customer's name, phone, need) for a caller
holding `sa:work:pii:see`. A share is `{n, of, percent, small_sample}`; `percent` is null while `of`
is under `min_sample` (§10.1), so the front end can only draw "n of of".

## Suspect leads

A landing's antispam sorts what its form receives three ways. A submission the honeypot
caught is a bot: the landing drops it and the panel never hears of it. One that is plausible
but doubtful is sent as an ordinary `lead.created` with `properties.suspect` set — it may be a
person, so it is kept rather than lost:

```text
suspect absent    an ordinary lead
"rate_limited"    the visitor's address sent more than the landing allows in its window
"too_fast"        the form came back sooner after it was shown than a person types
anything else     the event is rejected, like any word outside a closed vocabulary
```

The field is an extension of `LeadCreatedV1` (optional, `type_version` stays 1). A build
before it knows no such field and rejects an event carrying one, so the landings send it only
once the panel that takes it is live. The mark is the counted creation's (the first
journaled), so a later clean `lead.created` does not clear it, and progress does not either:
a suspect lead that is called and won is still one the antispam doubted.

Where it shows: `leads.suspect` (`TEXT`, NULL or one of the two words by CHECK), recomputed
like every column of the row, so the rebuild restores it from the journal; `suspect` on
`Lead` in `/api/v1` and the `suspect=only|exclude` filter of `GET /leads`; a `changed{topic:
leads}` like any new lead. `reporting_leads` carries the column; `reporting_funnel_daily`
still counts a suspect lead in `leads` and every stage it reaches, and counts it apart in
`suspect`, so a report that wants them out subtracts. Telegram tells of one under `new_lead`,
headed as suspect with its reason and with the usual buttons, and does not remind of it past
the contact SLA: nobody promised to call it back within 30 minutes.

## Flows and prices

A landing offers each need through one of three flows (FORM-VARIANTS-SPEC, lib#178): `quote`
(the customer asks for a price), `estimate` (a price computed from enum choices the visitor
made — zone, bedrooms, frequency, …) or `fixed` (a fixed price for a well-defined job). The
landing's server computes the price itself and says, in `lead.created`'s properties, what it
showed:

```text
flow                quote | estimate | fixed; absent: a landing from before the flows
quoted_cents        int64 ≥ 0, integer cents EUR TTC (no currency field yet)       ┐ both, exactly when
pricing_valid_from  RFC 3339 full-date, the day the pricing model took effect     ┘ flow is estimate|fixed
estimate_inputs     {input id: value id}, slugs of 1–40 [a-z0-9_-], ≤ 12 pairs; only with estimate
```

Anything else is rejected like any word outside a closed vocabulary: a price with `quote` or
no flow, a priced flow without both fields, one of the two alone, inputs with another flow, a
key or value not a slug (never free text: they sit in the clear), 13 pairs, a date that is
not a real day. The fields extend `LeadCreatedV1` (optional, `type_version` stays 1); a build
before them rejects an event carrying one, so kitstart sends them only once this panel is live
(its `panelFlow` switch).

Like `suspect`, they are the counted creation's (the first journaled): a later `lead.created`
saying nothing does not clear them. Where they show: `leads.flow`, `quoted_cents`,
`pricing_valid_from` (`TEXT`, a real day by CHECK) and `estimate_inputs` (JSON object), NULL
when the lead said nothing, rebuilt from the journal like every column; the same four on `Lead`
in `/api/v1` (null when absent) and the `flow=` filter of `GET /leads`. `reporting_leads`
carries the four; `reporting_funnel_daily` counts the `estimate` and `fixed` leads apart.

## Messenger leads

MESSENGER-CHANNELS-SPEC §3. A landing's messenger variants let the customer write on WhatsApp
(our prefilled message, ending `Réf. AQ-7K3F`) or open the brand's Telegram bot
(`t.me/<bot>?start=AQ-7K3F`) instead of leaving a phone number; a bot or an auto-responder may
start a lead from a conversation of its own.

```text
lead.created     channel += whatsapp | telegram; message_ref (optional): the ref the landing made,
                 ^[A-Z]{2,4}-[0-9A-HJKMNP-TV-Z]{4,8}$, not PII, not unique
lead.messaged@1  {channel: whatsapp|telegram, message_ref?}: the customer actually wrote. Writers
                 bot | panel (a landing only knows a link was opened). Subject: the lead, or
                 none and a ref → the brand's newest lead carrying it, looked up under the
                 journal's write lock and journaled as the event's lead_id (so a rebuild, when a
                 newer lead may carry the ref, does not look again; the content MAC is of the
                 event as sent, so a resend is a duplicate); no such lead yet → deferred
                 "unknown_ref: …" (409 + Retry-After, not journaled; the bot sends it again);
                 resolved by ref, it takes the lead's location when it named none. A leadId
                 with no lead yet: deferred "unknown_lead: …" the same way, never a phantom lead
source kind bot  a key per bot (`panel source add … --kind bot`): lead.created with a messenger's
                 channel only, lead.messaged, and the lookup by ref (the only kind that may)
```

On the lead: `leads.message_ref` (the counted creation's, indexed with the brand),
`messaged_at` and `messaged_channel` (the first message, both or neither); a message moves no
stage. `/api/v1` `Lead` carries `channel`, `message_ref`, `messaged_at`, `messaged_channel`;
`GET /leads` filters on `channel=` and `message_ref=`; an operator takes a messenger lead in by
hand (`POST /leads {channel}`) and says a customer wrote (`POST …/messaged`).
`reporting_leads` carries the three columns, `reporting_funnel_daily` counts `messaged`.
PostHog is told `sa_lead_messaged {channel}` once per lead, for the first message the panel
journaled, never the ref. An operator's `POST …/messaged` on a lead that has a message already
journals nothing and answers `200` with that message's id.

The place's `telegram` (its bot's username) and `messengers` (kill switches) are place
settings, below.

## Live updates, `/api/v1/live`

A screen reads through `/api/v1` and is told, over one WebSocket, when what it read may have
changed, so it reads again instead of being reloaded. Nothing is sent but the hint: the data
always comes from the API, under its checks.

```text
GET /api/v1/live    Origin = PANEL_PUBLIC_ORIGIN exactly (scheme, host, port)   else 403
                    the session, as the /api/v1 gate (cached GetMe)             else 401 / 503
                    sa:work:read (the Work section)                             else 403
                    ≤ 5 sockets per user, ≤ 200 in all                          else 429
                    → 101; no CSRF token (a GET), the Origin stands in for it
server → client     {"type":"hello","at","user_id"}                            at once
(JSON text frames)  {"type":"changed","topic","brand_id"?,"id"?,"at"}          after each commit
                    {"type":"resync"}                                          read everything again
                    WS Ping every 25 s; no Pong within 60 s → the socket is dropped
closes              4401 the session ended   4403 sa:work:read is gone   1001 the server is stopping
```

| `topic` | when | `brand_id` | `id` | who is told |
| --- | --- | --- | --- | --- |
| `leads` | a lead came in (`lead.created`: ingest, or typed in) | its brand | the lead | `sa:work:read` |
| `lead` | a stage, a call, a payment of one lead (the API or a Telegram button) | its brand | the lead | `sa:work:read` |
| `places` | a place's settings set, reverted, withdrawn, restored, or the place registered | its brand | the slug | `sa:work:read` |
| `pricing` | a brand's pricing saved or removed, or its locales set | its brand | — | `sa:work:read` |
| `sources` | a source key minted or revoked | — | — | `sa:admin:sources:manage` |
| `experiments` | a landing's declaration, or an admin's change | its brand | the key (a change) | `sa:analysis:read` |
| `telegram` | the user's link made, undone or found blocked; their rules | — | — | that user |
| `bookings` | a provider's booking without a lead came, changed, or was attached | its brand | — | `sa:work:read` |

A booking event tells `lead` for every lead it changed — a provider's booking attached
elsewhere tells the lead it left and the one it joined.

`at` is when the write committed. `resync` comes when the socket fell behind (below) and after
`panel rebuild-projections` in the serving process.

- **Who is told what they may read.** `panel::live::Change::visible_to` mirrors the reads:
  a topic is told to whoever holds the permission of the section that reads it, for every
  brand (no permission is narrower than the whole panel); a Telegram link is its user's. A
  narrower permission, if one comes, is one function to change.
- **Published after the commit, from the engine, in one place per kind of write.** Every
  event that reaches the projections passes through `Panel::journal`, which publishes once
  its transaction has committed — ingest, the operator API, the Telegram buttons and an
  admin's experiment changes alike, so none of them can forget to. A duplicate, a refused event or an
  unregistered type changed no read and says nothing. Outside the journal: a place's change
  (`place.rs`, after its transaction), a brand's pricing (`pricing.rs`, the same), the sources (`Panel::add_source`,
  `Panel::revoke_source`), the Telegram link (`telegram.rs`: `/start`, `/stop`, the profile's
  unlink, a chat found blocked, the rules). A write that changes nothing publishes nothing.
- **The bus is in-process** (`tokio::sync::broadcast`, one pod). Publishing never waits and
  never fails a write; with no socket open it is dropped. A command run in another process
  (`panel place set`, `panel source add`) is not seen by the
  server's sockets: the screens find it at their next read.
- **Bounded.** The bus keeps 1024 messages per subscriber; a socket further behind skips
  the backlog and is sent `resync`. A frame that does not go out within 10 s drops the
  socket; the socket's write buffer is capped at 256 KiB; a client frame is at most 4 KiB
  (the client has nothing to say but Pong and Close). Handshakes: 16 at once, 15 s each.
- **The session is asked again** every 60 s (the gate's `GetMe` cache is 60 s, so a permission
  revoked at concierge closes the socket within about two minutes), and at once when the
  engine closes a session: a sign-out (every socket of the user), a sign-in replacing the
  browser's session, concierge refusing a rotation or `GetMe`. Concierge unreachable keeps
  the socket, as the API keeps the session.
- **Not told:** time passing. A lead becomes overdue (`sla`) with no write; the screens keep
  their own clock for it.

## Place settings

A landing bakes its places into its build and lays over each, field by field, what the
panel answers for it (kitstart's `createPlaceSource`, `PlaceLive`): `phone`, `whatsapp`
(E.164), `telegram` (the place's bot, its username without the `@`:
`^[A-Za-z][A-Za-z0-9_]{1,28}[Bb][Oo][Tt]$`, 5–32 characters ending in bot), `messengers` (`{whatsapp?: bool, telegram?: bool}`, the
landing's messenger buttons switched off; absent is on, another key refused as
`messengers.<key>`), `hours` (`[{days, opens, closes}]`), `serviceArea` (commune names), and for
storefronts `address`, `geo`, `storefrontPhoto` (https), `landmark` (a text per locale),
`rating`. Hours are the place's local time, Europe/Paris for every place in v1 (no
time zone per place). kitstart fetches every 10 minutes at most, times out after 3 s, and keeps its baked
place on a 5xx or no answer; a JSON 404 is a place withdrawn, and the site 404s it.

```text
GET  /api/internal/brands/{brand}/locations/{slug}?locale   no session; ≤ 32 at once, 3 s
       200 the settings, only the fields set; {} for none, for a place the panel does not
       know, and for a brand or slug that cannot name one — never a 404 for those, or an
       empty database would take the sites down
       404 {"error": "not_found"}: only a place an admin withdrew
```

Under `/api/v1` (Work), CSRF on writes like the rest; writes need `sa:work:places:edit` and
ask concierge afresh (`Freshness::Fresh`); without it, a read (`can_edit` false, `403` on a write):

```text
GET  /places/{brand}/{slug}/settings          {brand, slug, withdrawn, settings, updated_at,
                                              updated_by, can_edit}; updated_at null: never set
PUT  /places/{brand}/{slug}/settings          {settings, expected_updated_at} → 200 as GET; a
                                              full replace; 409 {"error": "conflict"} when
                                              expected_updated_at is not the current one;
                                              422 {"error": "invalid", "fields": {key: why}},
                                              a key the field or its path in a list:
                                              hours[0].opens, hours[1].days, serviceArea[2]
GET  /places/{brand}/{slug}/settings/history  {changes: [{id, at, by, kind, before, after,
                                              reverts}]}, newest first; kind register | set |
                                              revert | withdraw | restore
POST /places/{brand}/{slug}/settings/revert/{id}  the `before` of that change made current,
                                              journaled as a revert → 200 as GET; 404; body
                                              {expected_updated_at} optional, 409 as a PUT
POST /places/{brand}/{slug}/withdraw|restore  → 200 as GET; the settings are kept
POST /places                                  {brand, slug} → 201 as GET; 409 {"error":
                                              "exists"} once registered (by hand or an edit)
```

- **Checked as kitstart reads them, refused instead of dropped.** kitstart drops a field it
  cannot read so a page never fails on one; the panel names it in a 422 so the editor sees
  why. Stricter than kitstart where kitstart is loose (a phone is E.164, not just `+…`;
  hours close after they open, a night across midnight is two rows; a day once per row),
  never looser. kitstart keeps a landmark only when it has every locale the site speaks,
  which the panel does not know.
- **A change is one write transaction**: the current settings read under the write lock,
  `expected_updated_at` compared, the new ones written, the change journaled
  (`place_changes`: before, after, who — the user's email and concierge id, or `cli` — and
  when). `updated_at` only grows, a microsecond at least per change, so two edits never share
  a token. A change that changes nothing writes nothing.
- **The CLI** (`panel place set|show|history|revert|withdraw|restore`) goes through the same
  calls, `by = cli`, patching over what is there when its transaction begins.
- **History is append-only, places never deleted**: triggers refuse any UPDATE or DELETE of
  `place_changes`, a DELETE of `places` or `place_settings` (cleared is `{}`), and any UPDATE
  of `places` but `withdrawn`.

## Pricing

A landing prices its `estimate` and `fixed` needs by a brand's price list (kitstart's
`PricingModel`, FORM-VARIANTS-SPEC, lib#178), baked into its build and replaced by what the
panel answers for the brand (`createPricingSource`: every 10 minutes at most, 3 s, the baked
model on any failure or on a model it refuses). **The model is kitstart's**: its shape, its
rules and its arithmetic are normative in kitstart's fixture README,
`crates/panel_core/tests/fixtures/pricing/README.md` (format 1, EUR TTC, integer cents,
basis points, every intermediate product rounded half up to the cent, the total to
`roundToCents`, then `minimumCents`; a `fixed` need as is). `panel_core::pricing` is a port of
`validate.ts` and `price.ts`, held to the same fixtures.

- **Vendored by sha.** `crates/panel_core/tests/fixtures/pricing/` is a copy of kitstart's
  `ts/kitstart/test/fixtures/pricing` at the commit `SOURCE` names; `tests/pricing_fixtures.rs`
  accepts every `valid/*.json` (and reads it back unchanged), refuses every `invalid/*.json` at
  the field its name says, and prices every `cases.json` to the cent, `null` included. kitstart
  changing a fixture changes the contract: copy the directory of the new commit over this one,
  write its sha in `SOURCE`, make the tests pass (adding the line a new invalid fixture needs),
  and ship the panel before the sites rely on it.
- **A brand's locales.** Every input's and option's label must be in each locale the brand's
  sites speak, or a site refuses the whole model; the panel refuses to save one (kitstart's
  `pricingProblemsFor`). The locales are `fr,en` unless set by `panel pricing locales <brand>
  fr,en` (`brand_locales`); a model saved before a locale was added is kept for the editor but
  not served (`{}`) until saved with the labels.
- **Paths.** A refusal names the first problem at kitstart's path without its `model.` root:
  `needs.standard.inputs[2]`, `inputs[0].options[1].labels`. JSON objects are read sorted here
  and in insertion order there, so with several problems the first named may differ; whether
  there is one never does. Saved, a model is stored in kitstart's shape (its maps' keys sorted).

```text
GET    /api/internal/brands/{brand}/pricing?locale   no session; with the locations' read ≤ 32
       at once (shed: `{}`), 2.5 s (then `{}`); `locale` ignored, every locale's labels sent
       200 the model; {} for none, for a brand id that cannot be one, for a model the brand's
       locales no longer pass, and on any failure of the store (logged) — never a 404 or 5xx
```

Under `/api/v1` (Work), CSRF on everything but GET; saving and removing need
`sa:work:pricing:edit` and ask concierge afresh (`Freshness::Fresh`); reading and the preview
are the section's:

```text
GET    /pricing                       {items: [Item]}: every brand the panel knows (a lead's,
                                      a place's, a count's, a source key's) or has pricing for
GET    /pricing/{brand}               Item; a brand never set: model, updated_at, updated_by null
PUT    /pricing/{brand}     edit      {model, expected_updated_at: RFC 3339 | null} → 200 Item;
                                      409 {error: "stale", current: Item}; 422 {error, path}
DELETE /pricing/{brand}     edit      {expected_updated_at} (required) → 200 Item, model null;
                                      409 as PUT
POST   /pricing/{brand}/preview       {model, need, inputs: {input: option}} → 200 {cents: n |
                                      null} (null: kitstart's "no price"); 422 as PUT: the draft
                                      is held to the brand's locales, as the site would
GET    /pricing/{brand}/changes       {changes: [{id, at, by, kind: set | remove, valid_from,
                                      needs, model}]}: the last 50, newest first; `model` the
                                      one the change left; a removal's three null

Item: {brand_id, locales, model | null, updated_at | null, updated_by | null}
```

- **A change is one write transaction**, as a place's: the current model and the brand's
  locales read under the write lock, `expected_updated_at` compared, the model checked and
  written, the change journaled (`pricing_changes`: before, after — NULL for none —, who, when).
  A removal keeps the row with `model` NULL and its own `updated_at`, which the next save names;
  `updated_at` only grows, a microsecond at least per change. Saving the model there is, or
  removing none, writes nothing and tells nobody.
- **The CLI**: `panel pricing show|history|set <brand> <file|->|remove|locales`, `by = cli`,
  over whatever is there when its transaction begins.
- **Append-only**: triggers refuse any UPDATE or DELETE of `pricing_changes` and a DELETE of
  `pricing`.

## Telegram (§8)

A bot (`TELEGRAM_BOT_TOKEN`; without it, or without the sign-in, none of this runs) writes to
each user in a private chat. Rules: `new_lead` and `contact_overdue` (`sa:work:leads:edit`, on
by default), `payment_received` and `source_silent` (`sa:admin:sources:manage`, off by
default), `booked` (`sa:work:leads:edit`, on by default: a slot booked, moved or canceled — "Бронь: <slot> (Paris)", "Бронь
перенесена", "Бронь отменена", "… без заявки" for one without a lead; not to the operator who
set or closed it themselves; no buttons), `access_requested` (every permission of `sa:admin`,
on by default: "🔑 Playbook access request" / "🔑 Access request", the email and name, and a URL
button "Grant access" to `<CONCIERGE_PUBLIC_ORIGIN>/en/cabinet/admin/allocations/service_arb?email=&target=`;
at most once a day per user and need). A 3★ review, a
funnel drop and Grafana alerts are variants to come, once their sources exist.

```text
POST /api/v1/telegram/link  GetMe asked afresh; 256 random bits, base64url; SHA-256 stored
                            with the caller's permissions and the time, 10 min, replacing the user's
                            earlier token → t.me/<bot>?start=<token>
/start <token>              private chats only, not forwarded (groups are ignored whatever they
                            say); the token redeemed once → telegram_links(user ⇄ chat, the
                            permissions confirmed as of the token's issue, the Telegram @username);
                            a chat linked to another account is refused ("/stop first"), the
                            token kept; the reply names the panel account
/stop, DELETE …/link        unlinked; what the outbox still owed them is dropped
fan-out (2 s)               new leads (their counted creation ≤ 1 h old, still `created`;
                            not to whoever typed one in; a suspect one headed "Suspect lead
                            (antispam: …)" instead of "New lead"),
                            leads created 30 min – 6.5 h ago never contacted, suspect ones
                            not (once each),
                            payments (≤ 24 h), sources silent ≥ 24 h (once per full day of
                            it) → telegram_fanout claims (rule, event) once, and in the same
                            transaction one outbox row per recipient, UNIQUE (rule, event, chat)
delivery (0.5 s)            lead messages queued > 1 h ago dead ("stale"); > 20 waiting for a
                            chat folded into one "N new leads, open the panel" without PII;
                            claim in one write transaction (nothing while the outbox is paused):
                            ≤ 25 tries started in any second, one per chat per second, none to
                            a chat with a send in flight; leased 60 s → the user's access
                            checked again (below) → sendMessage → sent | retry | dead
```

- **Updates by long polling.** A webhook would need a public route through the Cloudflare
  tunnel and a secret-header check on it; `getUpdates` needs egress to `api.telegram.org`
  only. One process polls, under a lease in `telegram_poller` (60 s, renewed every poll) that
  another takes over when it lapses; the offset is stored there after each update, and every
  update is idempotent, so one handled twice across a takeover does nothing twice.
- **Retries.** 429 pauses the whole outbox for `retry_after` and costs the message no try;
  5xx and timeouts back off from 10 s, doubling, to 15 min; 10 tries, then dead. 403 (the bot
  blocked) and 400 "chat not found" / "user is deactivated" mark the chat dead, give up what
  it was owed, and nothing more goes there until the user links again. Any other 4xx is dead
  at once; so is a queued message that no longer opens.
- **Quiet.** The bot answers commands only: other messages get nothing, and its unsolicited
  replies (help, "not linked", "invalid link") go at most once per chat per 10 min.
- **PII.** A new lead's message carries the brand, location, need and phone — for whoever
  holds `sa:work:pii:see`. Queued texts are sealed under
  `PANEL_DATA_KEY` like the journal's PII and dropped once sent or dead. What a customer typed
  is put on one line (control characters, line separators and bidi marks become spaces),
  bounded (name 80, need 500, phone 40 of digits and `+()-`, the message 3 500), and the name
  and need are `code` entities — no parse mode, so nothing is markup, no link is made of it,
  and no line of it can read as one of ours ("Взял: …"). Link previews are off.
- **Who gets a message: permissions concierge confirmed within the hour.** The panel learns
  them only from `GetMe`, which takes the user's own access token. Each link keeps the set last
  confirmed and when: the `/api/v1` gate records every `GetMe` answer, and every 15 min the
  bot asks again for a link not confirmed since, through the user's newest panel session
  used within 7 days (`sessions.last_seen_at`, set by the gate; rotated when due, as a
  request would). The set is checked again when a message is sent, not only when it is
  queued; a set that opens no rule gets nothing. A grant revoked at concierge, or a session concierge refuses to rotate (or whose
  `GetMe` it refuses twice running), stops messages at the next check and drops what was
  queued; no session used for a week, or concierge unreachable, stops them after an hour.
  The risk left: up to an hour of messages (with PII) to someone whose grant was revoked
  while concierge could not be asked.
- **Buttons.** "Взял" writes `lead.contacted`, "Не дозвонился" `call.attempted` +
  `call.logged{no_answer}`, through the operator API's path (`source.kind = panel`,
  `source.id` = the user). The callback data is `<button>.<outbox id>.<HMAC-SHA256, 80 bits>`
  under a key derived from `PANEL_DATA_KEY`, bound to the chat: nothing else in it is
  trusted. The message must be that chat's, the chat linked to the message's user, and the
  user's access is asked of concierge at the press — with no live panel session to ask with,
  the answer is "open the panel to confirm your access", never a cached answer; a press
  needs `sa:work:leads:edit`. The event ids
  derive from (user, outbox id, button), so a press repeated, or its update redelivered,
  records nothing twice. The message is then edited: "Взял: <preferred_name or email>" added
  and the buttons removed ("Не дозвонился" leaves "Взял").
- Language: `TELEGRAM_LOCALE` (`ru` default, `en`).

## Experiments

A landing's experiments live in its code (`@evinvest/experiments`); the panel holds them as
configuration — what each brand runs, and an admin's kill switch, weights and holdout over it.
Exposures, leads and their comparison are PostHog's: each experiment links to its funnel there.

```text
experiments.declared@1   the landing at every start (kind site, its key; a new id each time),
                         subject the brand alone: [{key [a-z0-9_]{1,64}, variants 2–32 unique
                         [a-z0-9_-]{1,32} (the first the control), weights one per variant ≥ 0
                         summing > 0, enabled, holdout? [0, 1), summary? ≤ 200}], ≤ 64, a key once
experiment.configured@1  an admin in the panel (kind panel, source.id the admin's id or cli),
                         subject the brand alone: {key, enabled?, weights?, holdout?, reset: [field], by —
                         the admin's email, else their id, as a place's history names them} — a
                         patch: a field absent left, named in reset put back to the declaration
```

Both are folded per brand (`panel_core::experiment::fold`, by `(occurred_at, id)`, so arrival
order does not matter) into `experiments`, recomputed in the transaction that journals either and
by the rebuild. The latest declaration is the brand's: an experiment it does not name is
`retired` — listed, never changed, never sent to a site. An override is kept as set; a field of
it is applied only while valid against the latest declaration, by the landings' own rules
(`applyOverrides`): weights as many as the variants, each ≥ 0, sum > 0; holdout in [0, 1);
enabled a boolean. So `effective` is what the landings run even after a declaration changed the
variants under an admin's weights. `weights_changed_at` is the last time the effective weights
changed, by a declaration or an admin.

```text
GET /api/v1/experiments?brand            Analysis; one brand's, or every brand's without it
     {experiments: [{brand, key, variants,
       declared: {weights, enabled, holdout, summary, declared_at},
       override: null | {weights, enabled, holdout (each null: follows the declaration),
                         changed_by (the admin's email, else their id: by), changed_at},
       effective: {weights, enabled, holdout}, weights_changed_at, retired, posthog_url}]}
PUT /api/v1/experiments/{brand}/{key}    sa:analysis:experiments:edit, fresh (as POST /sources); CSRF
     {enabled?: bool | null, weights?: [n] | null, holdout?: n | null}: absent left, null put
     back → 200 the item; 400 {error} invalid (weights against the declared variants); 404 an
     unknown or retired experiment, or a brand or key that cannot be one; a change that changes
     nothing journals nothing
GET /api/internal/brands/{brand}/experiments   no session, with the places' and pricing's reads
     ≤ 32 at once (shed: no overrides), 2.5 s (then none)
     200 {experiments: {key: {enabled?, weights?, holdout?}}}: the valid override fields of the
     current experiments; {experiments: {}} for none, an unknown brand, a brand id that cannot be
     one, any failure — never a 404 or 5xx
```

`posthog_url` is `{POSTHOG_APP_HOST}/project/{POSTHOG_PROJECT_ID}/insights/new#q=<query>`: an
`InsightVizNode` with a `FunnelsQuery` `experiment_exposed` → `experiment_lead`, event properties
`experiment = key`, `brand_id = brand`, `forced` not `true`, broken down by `variant`, from
`weights_changed_at` or the first declaration. Null without `POSTHOG_PROJECT_ID`.

## PostHog

The panel tells PostHog what happens to a lead after the form, so a visit and its payment are one
funnel there (`panel_core::analytics`):

```text
queued       Panel::journal, in the event's own transaction, when POSTHOG_PROJECT_API_KEY is
             set: a new lead.created (the one that counts), lead.messaged, lead.contacted, lead.quoted,
             job.won, lead.lost, job.completed, payment.received, call.logged, every booking.*
             → posthog_outbox
             never: the rebuild (it projects without passing there), a duplicate, a second
             lead.created, call.attempted, what names no lead (a provider's booking.created or
             booking.canceled no lead was matched to)
event        sa_ + the type, dots made underscores (sa_lead_created, sa_payment_received, …);
             booking.status_changed is sa_booking_closed
uuid         the journal's event id: PostHog deduplicates a resend
timestamp    occurred_at
distinct_id  the lead's analytics_id (lead.created's, the landing beacon's distinct_id), else
             sa-lead:<brand>:<lead>
properties   brand_id, location_id, manual; channel, flow, quoted_cents, suspect (created);
             channel (messaged, contacted); amount_cents, currency (quoted); reason (lost — the slug,
             never the note); billed_cents, commission_cents, currency (paid); outcome (call);
             provider, preferred_date, preferred_part (booking requested); provider, match,
             lead_time_hours (booking created: start_at − booked_at, else − occurred_at);
             provider (canceled); lead_time_hours (set); closed (closed); provider, match =
             manual (attached); nothing more (cleared).
             No PII: no need, name, phone, note, attendee; no external_ref, version or slot
sent (5 s)   ≤ 100 due rows → POST {POSTHOG_HOST}/batch/ → deleted; 5xx, 429, no answer: each
             row again from 10 s, doubling to 15 min; another 4xx: a batch is retried row by
             row, a row refused alone dropped (warned); a row failing for 7 days dropped (warned)
```

Without `POSTHOG_PROJECT_API_KEY` nothing is queued and nothing sent; `serve` warns and starts.
The key is the project's `phc_` key — public like the landings' — not a personal one (`phx_` is
refused at boot). A booking's slot is told as `lead_time_hours` alone: its weekday and part of
the day would need the place's time zone, which a fact does not carry.

### The retired import

Up to v0.3 an hourly HogQL import journaled `site.metrics`, `contact.metrics` and
`experiment.metrics` and projected them (`daily_location_metrics`, `daily_experiment_metrics`,
their `reporting_*` views, the `posthog_import` lease). The import and those tables are gone
(migration `20261005100000`); the three types stay registered, checked as before and projected
into nothing, so a journal holding them still passes and rebuilds.

## Booking

FORM-VARIANTS-SPEC, "Booking providers contract" (2026-10-04). Providers, a closed set:
`manual | link | google_calendar | cal_com` (`calendly` next, same seams). kitstart's booking
fixtures are vendored by sha in `crates/panel_core/tests/fixtures/booking` (`SOURCE`), their
README normative; `tests/booking_fixtures.rs` holds the config validator to `valid/` and
`invalid/`, `crates/panel/tests/booking.rs` the registry to `requested/`. `choose.json` and
`hrefs.json` are the site's (the panel neither picks a provider nor builds a link).

**A place's `booking`** (place settings, `PlaceLive.booking` in `/api/internal/…/locations`):
`{"default": <provider>, "providers": {<provider>: {"url"}}}`. `manual` is always available and
never a key of `providers`; `default` is `manual` or a key of it. URL rule (every page): ≤ 2048
printable ASCII, literal `https://`, no `#` nor `\`, no `@`/`:`/`[` in the authority, a dotted
DNS name whose last label is not a number; query allowed. `google_calendar`:
`calendar.app.google/…` or `calendar.google.com/calendar/appointments/…`; `cal_com`:
`cal.evinvest.ltd` or `cal.com`, path exactly `/<user>/<event>`; `link`: any host. A 422 names
`booking.default`, `booking.providers.<p>.url`, ….

**Journal types**

```text
booking.requested@1      site    {lead_ref = subject.lead_id (lead-<row>-<8hex>), provider,
                                 preferred_date?, preferred_part? (manual only)}; no null, no
                                 free text; the date 2 days back – 366 ahead of arrival (checked
                                 at ingest, not by the registry: a rebuild never re-judges it).
                                 Before its lead's lead.created: verdict `deferred`, the batch
                                 answered 409 + Retry-After 30 (kitstart retries 409/425 on it)
booking.created@1        booking {provider (with an adapter), external_ref, start_at, end_at?,
                                 match? (ref|contact, exactly with subject.lead_id), version,
                                 booked_at?}; the attendee in pii, sealed
booking.canceled@1       booking {provider, external_ref, version}
booking.set@1            panel   {start_at, end_at?}             → booked
booking.status_changed@1 panel   {status: done|no_show|canceled} → only from booked
booking.cleared@1        panel   {}                              → none
booking.attached@1       panel   {provider, external_ref}, subject.lead_id the lead
```

`source.kind = booking` is one kind for every adapter (the provider in the properties); no key
is ever issued for it (nor for `panel`). Migration `20261005090000_booking_source_kind` widened
the journal's CHECK by making `events` again: `-- no-transaction`, `PRAGMA foreign_keys = OFF`
outside a `BEGIN IMMEDIATE` copy, every row, index, trigger and `reporting_ingest_daily` made
again; its test checks the copy byte for byte, `foreign_key_check`, the triggers, and the way
down (refused while a `booking` event exists).

**Projections.** `booking_events` (a row per booking event), `bookings` (a provider's booking:
`(brand, provider, external_ref)`, its id derived from them; its slot, booked or canceled, its
lead and `match` — NULL while unmatched), and a lead's `booking_*` columns, folded
(`panel_core::booking::fold`) from its own booking facts and the events of the providers'
bookings joined to it now. Statuses `none | requested | booked | canceled | done | no_show`;
an operator's action not allowed from where it stands is a 409, and the fold passes over it
too, so a race lands where the first left it. A provider's change does not reopen a booking
an operator closed. A provider's booking is joined by: the latest `booking.attached`, else the
match its first event that named a lead was journaled with.

**The seam** (`panel::booking`). Push: `PushSource::verify(PushRequest{brand, headers, body,
now}) → Vec<BookingEvent>`, routed at `POST /api/hooks/booking/{provider}/{brand}` (no
session; 256 KiB, 8 at once, 10 s; 401 refused, 400 unreadable, 200 `{written, duplicate,
unchanged, ignored, refused}`); **no push provider is registered** — every provider answers 404
there. Pull: `PullSource::pull(brand, cursor, now) → {events, cursor}`, run by
`Panel::sync_bookings` under a lease per (provider, brand) in `booking_sync` (5 min; retried
after 1 min on failure); the cursor moves only once what it covers is journaled; a cursor the
provider dropped (`CursorExpired`) is a full pull. `Panel::ingest_bookings` journals each
`BookingEvent` with an id derived from (provider, brand, ref, type, revision): a revision seen
again is a duplicate; a slot as it stands already is `unchanged`; a cancellation of a booking
the panel never had is ignored.

**Matching**, once per booking (later events keep its lead): a `lead_ref` naming a lead of the
brand → `ref`; else the attendee's phone (E.164, kitstart's rule) or email (lowercase) against
the `phone` / `email` of the creation PII of the brand's leads created from 14 days before the
booking to 10 minutes after; one lead → `contact`, none or several → unmatched. The leads' PII
is opened under `PANEL_DATA_KEY` only to compare.

**google_calendar, pulled** (`panel_server::google_calendar`). Every minute, each brand with a
refresh token whose sync is due (`GOOGLE_CALENDAR_SYNC_MINUTES`, 5): the refresh token → an
access token (`oauth2.googleapis.com/token`, cached until a minute before it expires) →
`GET www.googleapis.com/calendar/v3/calendars/{id}/events?singleEvents=true&showDeleted=true`,
first `timeMin` = now − 7 days, then `syncToken`; pages followed (≤ 40 of 250); 410 → full
pull. An event is a booking (`looks_like_booking`, conservative, **unconfirmed against a real
appointment-schedule event**): timed, single (no recurrence), `eventType` default, a guest
with an email who is not `self`, the organizer or a room, not declining, and a marker in the
description — "Booked by" / "Réservé par", a schedule link, or a phone label ("Téléphone",
"Phone number", …; the schedule's form must ask for it). The phone is the line after (or
beside) its label. Version: the event's `etag`; moved: the same id with a new start; a
`cancelled` item is passed on by id (a sync gives its id alone).

**Secrets and CLI.** `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` (one client,
Desktop kind; both or neither), `GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND>` (secret, scope
`calendar.events.readonly`) and `GOOGLE_CALENDAR_ID_<BRAND>` (optional, `primary`); a brand
without a token is not pulled; a token without the client fails the boot. `panel booking
google-authorize <brand> [--port 8765]`: prints Google's consent URL (offline, prompt=consent,
PKCE, state), waits on `127.0.0.1:<port>` for the redirect, prints
`GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND>=…` once, for add-secrets. `panel booking google-sync
<brand> [--full]`: one pull now.

**API** (`/api/v1`, Work, CSRF on writes; writes need `sa:work:leads:edit`):

```text
POST /leads/{brand}/{lead}/booking         {action: "set", start_at, end_at?} | {action: "clear"}
                                           → 201 {event_id} (200 on an Idempotency-Key retry);
                                           400 bad instant; 404; 409 not from where it stands
POST /leads/{brand}/{lead}/booking/status  {status: done | no_show | canceled} → 201; 409 unless booked
GET  /bookings/unmatched?brand&limit       {bookings: [Booking]}, the next slot first, ≤ 200
POST /bookings/{id}/attach                 {lead} → 201 {event_id}; 404 no booking / no such lead
                                           of its brand; 409 attached to it by hand already
GET  /leads?booking=<status>               the leads whose booking stands there (none included)

Lead.booking: {status, provider, start_at, end_at, external_ref, match, preferred_date,
               preferred_part}
Booking:      {id, brand, provider, external_ref, status: booked | canceled, start_at, end_at,
               booked_at, last_event_at, lead_id: null, match: null,
               contact?: {name?, email?, phone?} (for a caller who sees PII)}
```

## Invariants

- **The journal is append-only.** `events` rows are inserted once. Triggers refuse `DELETE`,
  `TRUNCATE`, and any `UPDATE` but of `status`/`status_reason`, which the rebuild rewrites when
  the registry has changed its mind about an event. Everything else is derived from it.
- **An event is its id.** The source issues a UUIDv7; the journal is keyed by it. A resend is
  `duplicate`; the same id with other content (compared as a MAC of the event in canonical
  form, so field spelling and key order do not matter, under a key derived from
  `PANEL_DATA_KEY`, so a dump cannot confirm a guess at the PII it covers) is `rejected`. Delivery is at-least-once
  on the sources' side, exactly-once in the journal.
- **Unknown types are kept, not dropped** (§3.2): stored as `unregistered`, not projected, and
  judged again by `panel rebuild-projections` — which is how a type registered later picks up
  what arrived before it. Known types are checked strictly: unknown fields, a wrong vocabulary
  word, a lead event without a lead are rejected, and not journaled.
- **A key writes what it was registered for.** Each source key has one `kind` and a set of
  brands, and names its source: `source.id` must be the key id. An event claiming another
  source, another brand, or another kind (a site key cannot pass its events
  off as typed in by hand), is rejected. Events of kind `panel` are the manual ones (§10a), and
  every projection row carries `manual`.
- **A kind writes only its types** (`panel_core::event::may_write`): `lead.created` from `site`
  or `panel`; `lead.contacted` and the calls from `panel` or `telephony`; quotes, wins, losses,
  completed jobs and payments from `panel` alone (payments are entered by hand). Unknown types
  are open to every kind. Checked on the key at ingest and again by the registry, so the
  rebuild drops anything that slipped in.
- **A lead is created once.** Of several `lead.created`, the first the panel journaled counts
  (by `received_at`, then id) and the others are left out of the projection, so a source
  cannot back-date a creation to take over someone else's lead.
- **The timestamp is signed.** The MAC covers `sa-ingest/v1.<timestamp>.<body>`, so the 5-minute window
  cannot be dodged by re-stamping a captured request (the weakness concierge's Didit handler
  works around). An unknown key and a bad signature answer the same.
- **Projections are functions of the journal.** A lead is never patched: every event about it
  recomputes its row from all its registered events (`panel_core::lead::fold`, ordered by
  `(occurred_at, id)`, so arrival order does not matter), inside the transaction that
  journaled the event. The rebuild runs the same code in one transaction, so it lands on the
  same state; the tests check that it does. Ingest and the rebuild do not interleave, nor two
  events of one lead: every transaction that writes begins `BEGIN IMMEDIATE`, taking SQLite's
  one write lock up front, so writers queue (up to `busy_timeout`, 10 s) and readers, in WAL,
  go on reading the last commit. Calls and payments are one row per event.
- **Stages move forward** through created → contacted → quoted → won → completed → paid; a
  `lead.lost` moves a lead to `lost` from anywhere, and later progress reopens it. Stage times
  are when each stage was first reached.
- **PII never sits in the clear.** `pii` is sealed per event (XChaCha20-Poly1305, the event id
  as associated data) under `PANEL_DATA_KEY`, whose fingerprint is stored beside every blob.
  The free text a customer typed goes in `pii`, not `properties`. Sources' HMAC secrets are
  sealed the same way (they must be usable to verify, so they cannot be hashed).
- **Reporting has no PII.** The `reporting_*` views over the projections and a daily ingest
  count select no `properties`, `pii_sealed` or secret: what anything reading a copy of the database (a dashboard on the replica, an export)
  should be pointed at instead of the tables. SQLite has no roles to enforce that; whoever
  holds the file holds everything, sealed PII included — which is why PII is sealed.
- **The schema guards the journal.** SQLite has no roles, so what a runtime role's grants
  said on Postgres the tables' triggers say: `events` refuses DELETE and any UPDATE but of
  `status`/`status_reason` (compared NULL-safe, so writing a value back unchanged passes);
  `sources` refuses DELETE and any UPDATE but of `revoked_at`. Tables are STRICT, foreign keys
  enforced (`PRAGMA foreign_keys = ON` on every connection), and every Postgres CHECK kept.
- **One file, migrated on open.** `PANEL_DB_PATH` names it (`/data/panel.db` in the image);
  every command, `serve` included, creates it if missing and applies the migrations it lacks
  before anything else (`panel migrate` does only that). A migration newer than the build
  is let be: that is a rollback onto a schema moved on. Timestamps are INTEGER microseconds
  since the epoch, days `YYYY-MM-DD` text, UUIDs 16-byte blobs, JSON text that must parse.
- **Secrets come from the environment only** (`PANEL_DATA_KEY`, `SENTRY_DSN`,
  `RP_CLIENT_SECRET_SA`, `PANEL_ASSERTION_KEY`, `TELEGRAM_BOT_TOKEN`, `GOOGLE_OAUTH_CLIENT_SECRET`,
  `GOOGLE_CALENDAR_REFRESH_TOKEN_<BRAND>`),
  through `ev_lib::settings`; with `APP_ENV=production`, `PANEL_DB_PATH`, `PANEL_DATA_KEY` and
  the four sign-in variables and the forward's three are required at boot
  (`panel --print-required-vars` lists them).

## Deploy requirements

- **One image, one origin.** The image carries the binary and the front end's static
  export, and sets `PANEL_WEB_DIR` to it; `serve` answers `/api`, `/auth` and `/health`
  first, then the forward's prefixes (Forward), then the files (a directory by its
  `index.html`, anything else `404.html` with a 404). `/grafana` is held: a JSON 404 until the dashboards move there; so is `/cabinet` beyond the chip's files (Forward). `/_next/static/*`
  is `immutable` for a year, everything else `no-cache` (the store's 1970 mtimes make
  date revalidation meaningless, so the panel answers none); every page carries a CSP of
  `default-src 'self'` with `'unsafe-inline'` for scripts and styles (Next's inline
  bootstrap), `'wasm-unsafe-eval'` (the review_archive dashboard), `frame-ancestors 'none'`, `Referrer-Policy: same-origin`. Without
  `PANEL_WEB_DIR`, `serve` answers the API alone and says so.
- **The platform's MFE bundles (`PANEL_CSP_PLATFORM_MFE`).** `0` (default, also unset) leaves
  the CSP above exactly as it was; `1` (anything else fails the boot) adds to `script-src` the
  two directories `https://evinvest.ltd/api/auth/mfe/` and `https://evinvest.ltd/wallet/mfe/`,
  and to `connect-src` the origin `https://evinvest.ltd` (the elements call its API); styles,
  images and fonts do not change. The string is built once at boot (`web::csp`, kept in
  `web::Files`). Directories, not the origin, on purpose: the pins on `/auth/mfe` and
  `/cabinet/mfe` (Forward) vouch for the bytes, and a `script-src` of the whole origin would
  get around them for any script that origin serves, so the pins are not to be bypassed that
  way. The host loads the bundles by the platform's manifest (the front end's
  `NEXT_PUBLIC_MFE_PLATFORM=1`, switched on together with this). The proxy `/auth/mfe/*` and
  its pins stay as the fallback until this is proven in production; a separate change removes
  them. The sign-in's own pages (`PAGE_CSP`, above) host no element and are not widened.
- **The contract** (`nix eval .#containers.<system>.panel.contract`) names the port
  (59120), `/health`, `APP_ENV=production`, the variables required, secret and optional,
  `PANEL_DB_PATH`, the volume (`mounts = [ "/data" ]`) and the SQLite file on it to replicate
  (`sqlite = [ "/data/panel.db" ]`), what the ingress must not publish or must rate-limit, and
  the egress the pods need. `checks.contract-env` fails the flake when its
  list of required variables and the binary's `--print-required-vars` disagree.

- **`/api/internal` stays inside the cluster.** The landings read their places' settings,
  their brand's pricing and its experiments' overrides there by service DNS; it has no session, so the IngressRoute must exclude the prefix
  (`excludePathPrefixes`) and the NetworkPolicy admit only the landings' pods.
- **Ingest stays inside the cluster.** Its sources (the landings, review_archive) reach it
  by service DNS (§3.3); the IngressRoute that publishes `sa.evinvest.ltd` must not route
  `/api/ingest`, and the NetworkPolicy lets in only the pods that send. The signature is
  what authenticates a batch; keeping the route off the internet is what keeps its cost —
  a database lookup and a MAC per request — away from anyone who can reach a URL.
- **concierge within reach.** The panel's pods call concierge's gRPC (`CONCIERGE_GRPC_ADDR`)
  for every sign-in, token rotation and `GetMe`; the egress policy must allow it. concierge
  must register client `sa` with redirect URI `<PANEL_PUBLIC_ORIGIN>/auth/callback` exactly,
  and hold the hash of `RP_CLIENT_SECRET_SA`; `serve` publishes the `sa` catalog there at
  every boot and does not start if it is refused.
- **The cabinet within reach.** With `PANEL_CABINET_URL` (a bare `http://` origin, e.g.
  `http://ev-banking-cabinet.apps:50061`; optional, not a secret) the pods need egress to it,
  and the cabinet's ingress policy, if it has one, must let the panel in. It is served only
  with `PANEL_CABINET_MFE_PINS` too (Forward; not a secret, no egress of its own). Either
  unset, `serve` warns and `/cabinet/mfe/*` is a JSON 404; malformed, the boot fails.
- **concierge's web port within reach.** Only with `PANEL_CHIP_SOURCE=concierge`: the pods
  need egress to `PANEL_CONCIERGE_URL` (concierge's web port, beside its gRPC one), and
  `PANEL_CONCIERGE_MFE_PINS` (Forward; not a secret). Open question before this is switched
  on in production: the page's CSP has `connect-src 'self'` (via `default-src`), while the
  chip's `/api/auth/session` call goes to concierge's origin, cross-domain. Only
  `connect-src` may be widened for it, never `script-src`: the pin is what vouches for the
  scripts, and a wider `script-src` would let any script from that origin around it.
- **Telegram, outbound only.** With `TELEGRAM_BOT_TOKEN` (the panel bot's, in sops; not
  `telegram_token_main`) the pods need egress to `api.telegram.org:443`; nothing inbound. No
  webhook is set on the bot (`getUpdates` refuses to run while one is).
- **PostHog, outbound only.** With `POSTHOG_PROJECT_API_KEY` the pods need egress to
  `us.i.posthog.com:443` (or wherever `POSTHOG_HOST` points). The key is the project's public
  `phc_` key, not a secret. The experiments' links (`POSTHOG_PROJECT_ID`, `POSTHOG_APP_HOST`)
  are opened by the browser: no egress for them.
- **Google, outbound only.** With the booking pull configured the pods need egress to
  `oauth2.googleapis.com:443` and `www.googleapis.com:443`. `/api/hooks/booking` is public
  (no push provider is registered yet: 404s) and rate-limited per IP at the edge.
- **`/api/v1/live` through the public IngressRoute, as a WebSocket.** Traefik proxies the
  upgrade as is; nothing in front of it may buffer the response or strip `Upgrade` /
  `Connection` / `Origin`, and an idle timeout on the way must be longer than the 25 s ping
  (Cloudflare's 100 s is). The `/auth` rate limit does not cover it.
- **Rate-limit `/auth` per client IP at Traefik** (a `RateLimit` middleware on the
  IngressRoute's `/auth` prefix, e.g. 10/min with a burst of 20). The panel bounds how many
  sign-ins run at once, not who starts them; per-IP limits are the edge's.
- **One pod, a volume, litestream.** The database is the file on `/data`, a volume writable
  by uid 65534 that the cluster replicates off the pod with litestream (to R2) and restores
  on an empty volume, as for the tenant's other apps. One writer: one replica, rolled out
  with `Recreate`, never two pods on the volume at once. No init container: `serve` migrates
  on start, so rolling out an image with a new migration applies it, and rolling the image
  back does not undo it.
