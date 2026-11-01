# Running the stack locally

The conductor with its Grafana, sites_panel, review_archive and both landings (aquafix, vifnet)
on one machine, wired as in the cluster: a lead posted on a landing reaches sites_panel through
the conductor's ingest, a place's phone or hours edited on the sites page shows on the landing
through sites' `/api/internal`, review_archive's overview and boards draw in the conductor's
Grafana, and the conductor's GMBs and tokens reach review_archive. No concierge, no PostHog, no Telegram, no playbook (its forward answers 502), no Google
(review_archive scans nothing): the conductor signs you in by itself (dev sign-in, below).

## Prerequisites

- Nix with flakes. Everything else (Rust, Node, the sites' toolchains) comes from the flakes.
- The others checked out beside this one, as `_sa/` holds them:

  ```text
  _sa/
    site_conductor/  this repository (its worktrees work too: the others are found beside the main checkout)
    sites/panel/     sites_panel
    review_archive/  review_archive (skipped, with a message, when missing)
    aquafix/         Service-Arb/aquafix, up to date — the Next.js (kitstart) site
    vifnet/          Service-Arb/vifnet, likewise
  ```

  Elsewhere: `SITES_DIR=… REVIEW_ARCHIVE_DIR=… AQUAFIX_DIR=… VIFNET_DIR=… nix run .#local-stack`. A landing checkout
  without `package.json` (the old Rust sites) is skipped with a message: `git pull` it.
- Ports 59120 (conductor), 59121 (sites_panel), 59110 (review_archive), 59081 (aquafix) and 59082 (vifnet) free.

## The command

```sh
nix run .#local-stack                  # build, seed, start all four; Ctrl-C stops everything
nix run .#local-stack -- --reset       # wipe .local/ first: databases, keys, the landings' leads
nix run .#local-stack -- --no-sites    # the conductor and sites_panel, no landings
nix run .#local-stack -- --role operator
nix run .#local-stack -- --watch       the front end from `next dev`: open http://localhost:3120
```

`--watch` builds no front end: `next dev` serves this checkout's `frontend/` on
<http://localhost:3120>, reloading as it is edited, and proxies `/api`, `/auth`, the bundles and
`/grafana` to the conductor, whose public origin it becomes (the sign-in returns there, the live
socket comes from there).

The first run builds the conductor, sites_panel and their front ends with Nix (minutes), and
each landing's `nix run .#dev` installs its `node_modules` (minutes again). Later runs start in
seconds.

What it does (`scripts/local-stack.sh`, which the flake app runs as is):

1. Keeps its state in `.local/` (ignored by its own `.gitignore`): `panel.db` and
   `sites_panel.db`, the `data-key` both share, the sources' secrets in `secrets/`, each
   landing's `<site>-leads.db`, review_archive's data dir in `review_archive/`, and the logs in
   `logs/`. It persists between runs; `--reset` wipes it. A fresh `panel.db` imports an empty
   document from review_archive, which opens tokens and GMBs.
2. Seeds, idempotently: the sources `aquafix-site` (brand `aquafix`, origin
   `http://localhost:59081`) and `vifnet-site` (brand `vifnet`, origin `http://localhost:59082`)
   in the conductor, each secret kept in `.local/secrets/` and reused, a key from an older run
   given its origin; the
   places `aquafix/{royat, clermont-ferrand, desgenettes, lyon-est, la-mouche, lyon-nord}` and
   `vifnet/vifnet` in sites_panel, registered with nothing set (`sites_panel place register`).
3. Starts `sites_panel serve` on `127.0.0.1:59121` (its bundle from `PANEL_MFE_DIR`, the
   conductor's public key in `PANEL_ASSERTION_KEYS`), `review_archive serve` on `127.0.0.1:59122`
   likewise (a headless Chromium on a signed-out profile: its Maps walks fail and say so), then
   `panel serve` on `127.0.0.1:59120`
   in development, with dev sign-in, a signing key for this machine alone, `PANEL_SITES_URL`
   at sites_panel, and the front end's static export served on the same origin.
4. Starts each landing with its own `nix run .#dev` (kitstart's `next dev`), with:

   | Variable | Value |
   | --- | --- |
   | `LEAD_WEBHOOK_URL` | `http://127.0.0.1:59120/api/ingest/v1/events` |
   | `LEAD_WEBHOOK_KEY_ID` / `LEAD_WEBHOOK_SECRET` | the seeded source's |
   | `SITES_PANEL_INTERNAL_URL` | `http://127.0.0.1:59121/api/internal` |
   | `LEADS_DB_PATH` | `.local/<site>-leads.db` |
   | `POSTHOG_KEY` | unset: analytics off |

5. Prints the URLs, prefixes every log line with whose it is, and stops everything on Ctrl-C
   (or when the conductor or sites_panel stops). Started from a non-interactive shell (an agent's, in the
   background), SIGINT does not reach it: stop it with `kill <pid>` (SIGTERM), which does the
   same. Each part runs in a process group of its own, so the caller's shell is never hit.

Iterating on the backend: `cargo build -p panel_server`, then
`PANEL_BIN=$PWD/target/debug/panel nix run .#local-stack` (the same as
`scripts/local-stack.sh`): what you do not name is built with `nix build`. `SITES_BIN` and
`SITES_MFE_DIR` do the same for sites_panel, `REVIEW_ARCHIVE_BIN` and `REVIEW_ARCHIVE_MFE_DIR`
for review_archive, `PANEL_GRAFANA_SERVER` for Grafana (`nix build .#grafana-server`).

review_archive runs `serve --no-login-check` on `.local/review_archive/` (its config written
there; `REVIEW_ARCHIVE_DATA_DIR` names another data dir, e.g. a copy of its checkout's `data/`),
trusting the stack's assertion key, HTTP only: the stack holds the dir's `serve.lock`, so nothing
walks Maps; the conductor runs Grafana on `.local/grafana/`,
pulling review_archive's dashboards into it.

## URLs

| What | URL |
| --- | --- |
| Conductor | <http://127.0.0.1:59120> — opens signed in |
| Leads, locations | <http://127.0.0.1:59120/sites/leads>, <http://127.0.0.1:59120/sites/places> |
| Sources, connected sites | <http://127.0.0.1:59120/sources> (with `--role admin`) |
| sites_panel | <http://127.0.0.1:59121> — `/api/internal` only without an assertion |
| review_archive, drawn by Grafana | <http://127.0.0.1:59120/review_archive?grafana=true> |
| Grafana | <http://127.0.0.1:59120/grafana/> |
| aquafix, a point | `http://<slug>.localhost:59081/fr`, e.g. <http://royat.localhost:59081/fr> |
| aquafix, the brand page | <http://localhost:59081/fr> |
| vifnet | <http://localhost:59082/fr> |

`*.localhost` resolves to this machine in every current browser and in curl; nothing to add
to `/etc/hosts`.

## Dev sign-in

`PANEL_DEV_SIGN_IN` — an alias (`sa:admin`, `sa:operator`), a comma-separated list of `sa`
permissions, or `none` — (and optionally `PANEL_DEV_SIGN_IN_EMAIL`, by default
`dev-admin@localhost` for `sa:admin`) replaces concierge: `/auth/login` sends the browser
straight to `/auth/callback`, which opens a real session for a made-up user holding
those. An unknown permission fails the boot, named. The account menu shows the user as **Dev
sign-in (dev-admin@localhost)**, `GET /api/v1/me` answers
`"dev_sign_in": true` (for a UI badge), and `serve` logs `DEV SIGN-IN ON` at start.

The binary refuses it at start (exit 78, every command) in any `APP_ENV` but `development`,
beside any concierge variable, and unless `PANEL_PUBLIC_ORIGIN` is `http://localhost[:port]`
or `http://127.0.0.1[:port]` — so it cannot be switched on in the image, which sets
`APP_ENV=production`. `cargo test -p panel_server --test dev_sign_in` holds the binary to it.

Without the script, the conductor alone (no forward, so no ingest and no sites):

```sh
APP_ENV=development PANEL_DB_PATH=./panel.db PANEL_DATA_KEY="$(panel gen-data-key)" \
PANEL_PUBLIC_ORIGIN=http://127.0.0.1:59120 PANEL_DEV_SIGN_IN=sa:admin \
PANEL_WEB_DIR="$(nix build .#frontend --print-out-paths)" panel serve
```

The front end's dev server instead of the export: start the stack, then
`cd frontend && PANEL_DEV_BACKEND=http://127.0.0.1:59120 npm run dev` serves on `:3120`. The
sign-in's redirect goes to `PANEL_PUBLIC_ORIGIN`, so for cookies on `:3120` run the conductor by
hand as above with `PANEL_PUBLIC_ORIGIN=http://localhost:3120`. `npm run dev:stub` in
`frontend/` stands in for the backend instead.

A session in curl (what a browser does):

```sh
jar=$(mktemp)
curl -s -c "$jar" -b "$jar" -L -o /dev/null http://127.0.0.1:59120/auth/login
curl -s -b "$jar" http://127.0.0.1:59120/api/v1/me
# sites' API through the forward; a write needs the CSRF cookie echoed in x-sa-csrf, and
# expected_updated_at is the place's current updated_at (GET …/settings), null for one never set
csrf=$(awk '$6 == "sa_csrf" { print $7 }' "$jar")
curl -s -b "$jar" -H "x-sa-csrf: $csrf" -H 'content-type: application/json' \
  -X PUT http://127.0.0.1:59120/api/sites/places/aquafix/royat/settings \
  -d '{"settings": {"phone": "+33499887766"}, "expected_updated_at": null}'
```

Or skip the session: `sites_panel place set aquafix royat --phone +33499887766` with the stack's
`PANEL_DB_PATH=.local/sites_panel.db PANEL_DATA_KEY=$(cat .local/data-key)`.

## The end-to-end check

With the stack up:

**A lead, landing → sites**

1. Open <http://royat.localhost:59081/fr> and submit the quote form (any valid French mobile,
   e.g. `0612345678`, and a postcode such as `63130`). The site answers with its thanks page.
2. The site keeps the lead in `.local/aquafix-leads.db` and its outbox posts it, signed, to
   the conductor within about 10 s (kitstart's `WEBHOOK_TICK_MS`), which forwards it to
   sites_panel: `[sites]` logs `ingested a batch key_id="aquafix-site" accepted=1`; the site's
   side is the row in
   `sqlite3 .local/aquafix-leads.db 'select id, state, attempts, last_error from webhook_outbox'`
   (`delivered`).
3. Open <http://127.0.0.1:59120/sites/leads>: the lead is there, stage *created*, brand aquafix,
   location royat, with what you typed — it came in live.

The same for vifnet at <http://localhost:59082/fr>. Without a browser, the form's no-JS path:

```sh
t=$(( $(date +%s) * 1000 - 5000 ))   # rendered five seconds ago, as a person would have
curl -si http://royat.localhost:59081/quote --data-urlencode locale=fr --data-urlencode form_id=quote \
  --data-urlencode t=$t --data-urlencode location=royat --data-urlencode job=blocked_drain \
  --data-urlencode zip=63130 --data-urlencode mobile=0612345678 | head -1      # 303 → /fr/thanks
t=$(( $(date +%s) * 1000 - 5000 ))
curl -si http://localhost:59082/quote --data-urlencode locale=fr --data-urlencode form_id=quote \
  --data-urlencode t=$t --data-urlencode location=vifnet --data-urlencode subject=deep \
  --data-urlencode locality=83702 --data-urlencode mobile=2085550192 \
  --data-urlencode 'name=Smoke Test' --data-urlencode bedrooms=3 | head -1
```

**A board, review_archive → Grafana**: `node examples/with_grafana.cjs` in review_archive's
checkout drives a headless browser through the overview and a place's board (its header says
what it checks).

**A place, sites → landing**

1. Open <http://127.0.0.1:59120/sites/places>, pick aquafix / royat, set a phone (e.g.
   `+33 4 23 50 06 40`) and save.
2. `curl -s http://127.0.0.1:59121/api/internal/brands/aquafix/locations/royat` answers the
   new phone at once: that is what the site reads.
3. Reload <http://royat.localhost:59081/fr>: the new number shows — see the cache note below.

**Connected sites, landing → conductor**

1. `curl -s http://localhost:59081/.well-known/sa-site` answers aquafix's own report: the state of
   each place, of pricing and of experiments as it reads them from sites_panel (`live`, `empty`,
   `withdrawn`; `down` with why, `unset` without `SITES_PANEL_INTERNAL_URL`).
2. Open <http://127.0.0.1:59120/sources>: the Connected sites table shows both keys with that
   report, kitstart's version, the last event sites took from each key and the last time each
   brand's data was read from sites' `/api/internal`.

**The cache.** kitstart fetches a place with `cache: "force-cache"` and a 600 s revalidate
(`PLACE_REVALIDATE_SECONDS`), and the point pages are ISR with `revalidate = 600`. In
production a change therefore shows within about 10 minutes (the first request after the
TTL re-renders in the background, the next one gets it). Under `next dev` the fetch is cached
the same way (in memory, 600 s): a plain reload keeps the old number. To see a change at once:

- a **hard reload** (Cmd-Shift-R / Ctrl-Shift-R): the browser sends `Cache-Control: no-cache`,
  and `next dev` then fetches afresh — later plain reloads get the new value too. In curl:
  `curl -H 'cache-control: no-cache' http://royat.localhost:59081/fr`;
- or restart the stack (the cache is in memory).

Production has no such bypass: there, wait out the 600 s.

## Troubleshooting

- **`port 59120 is taken`**: another stack, or something else —
  `lsof -nP -iTCP:59120 -sTCP:LISTEN` names it. The same for 59121/59110/59081/59082.
- **A site is skipped**: its checkout is missing or predates the Next site; `git pull` it, or
  point `AQUAFIX_DIR` / `VIFNET_DIR` at another one.
- **`source … exists but its secret is not in .local/secrets`**: the secret is shown once and
  `.local/secrets/` lost it. `--reset`.
- **The lead does not arrive**: look at the outbox row (`webhook_outbox` in
  `.local/<site>-leads.db`): `last_error` holds the answer, and the outbox retries with
  backoff, so a lead posted while the conductor or sites_panel was down arrives after it is
  back. A `401` means the site's key id and secret are not the conductor's — e.g. a site started
  by hand with old values after a `--reset`; the script always passes the current ones.
- **The site shows the old phone**: the cache, above. A `404` for the place in sites'
  answer means it was withdrawn (Locations → restore).
- **The sites' checkouts get `AGENTS.md` and `CLAUDE.md`**: `next dev` writes them unless a
  site's `next.config.ts` sets `agentRules: false` (the conductor's front end does).
- **`nix build .#frontend` hangs** in `panel-frontend-…-npm-deps` (seen on a Mac whose Nix
  daemon could not reach the npm registry from its fetcher): build the export with npm
  instead and name it — `(cd frontend && npm ci && npm run build)`, then
  `PANEL_WEB_DIR=$PWD/frontend/out nix run .#local-stack`.
- **Start over**: `nix run .#local-stack -- --reset`.
