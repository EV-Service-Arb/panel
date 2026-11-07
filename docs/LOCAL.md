# Running the stack locally

The conductor with its Grafana, sites_panel, review_archive and both landings (aquafix, vifnet)
on one machine, wired as in the cluster: a lead posted on a landing reaches sites_panel through
the conductor's ingest, a place's phone or hours edited on the sites page shows on the landing
through sites' `/api/internal`, review_archive's overview and boards draw in the conductor's
Grafana, and the conductor's GMBs and tokens reach review_archive. No concierge, no PostHog, no Telegram, no playbook (its forward answers 502), no Google for review_archive
(review_archive scans nothing): the conductor signs you in by itself (dev sign-in, below).

## Prerequisites

- Nix with flakes. Everything else (Rust, Node, the sites' toolchains) comes from the flakes.
- The others checked out beside this one, as `_sa/` holds them:

  ```text
  _sa/
    site_conductor/  this repository (its worktrees work too: the others are found beside the main checkout)
    sites/panel/     sites_panel
    review_archive/  review_archive (skipped, with a message, when missing)
    sites/aquafix/   Service-Arb/aquafix, up to date — the Next.js (kitstart) site
    sites/vifnet/    Service-Arb/vifnet, likewise
  ```

  Elsewhere: `SITES_DIR=… REVIEW_ARCHIVE_DIR=… AQUAFIX_DIR=… VIFNET_DIR=… nix run .#local-stack`. A landing checkout
  without `package.json` (the old Rust sites) is skipped with a message: `git pull` it.
- Each part's port free: the one its own dev shell sets (docs/port_range.md).

## The command

```sh
nix run .#local-stack                  # build, seed, start all four; Ctrl-C stops everything
nix run .#local-stack -- --reset       # wipe .local/ first: databases, keys, the landings' leads
nix run .#local-stack -- --reset --from-prod   # prod's data in place of a fresh state (below)
nix run .#local-stack -- --no-sites    # the conductor and sites_panel, no landings
nix run .#local-stack -- --role operator
nix run .#local-stack -- --watch       # every part rebuilt and restarted as its checkout is edited
nix run .#local-stack -- --port-offset <n>   # every port moved by n: a second stack, in .local-<n>/
```

A stack already up on the ports is reported, and the command exits: use it. The URLs to open
are what it prints; below, `<conductor>`, `<sites_panel>`, `<aquafix>` and `<vifnet>` stand for
those origins.

`--watch` builds every part from its checkout instead of with Nix: the conductor, sites_panel
and review_archive with cargo, each restarted when its crates change; review_archive's and the
sites' bundles rebuilt in place as their sources change (a reload shows them); the conductor's
front end from `next dev` on this checkout's `frontend/`, which proxies `/api`, `/auth`, the
bundles and `/grafana` to the conductor and is the origin to open (the sign-in returns there, the
live socket comes from there). The landings are `next dev` either way.

### `nix run .#dev`

```sh
nix run .#dev                  # the conductor, sites_panel and review_archive under --watch, no landings
nix run .#dev -- --port <n>    # the conductor on n and its next dev on n+1, beside the usual one
```

Each part already answering `/health` on its port is used as it is; the rest are started under
`--watch`, and Ctrl-C stops only those. The peers are the services whose bundles the front end
mounts — `devPeers` in flake.nix — each from `../<name>` beside the main checkout; one not there
is skipped with a warning. `devUsers` in flake.nix gives a user (`id -un`) another `path` for a
peer (relative to the main checkout, or absolute), or a `command`, run as is in the peer's
checkout in place of the stack's own serve, which must answer on the port its dev shell sets and
watch by itself. `SITES_DIR` and `REVIEW_ARCHIVE_DIR` still win. `--port` takes two ports of
`SA_PORT_RANGE` (docs/port_range.md) and a state of its own, `.local-port-<n>/`: two conductors
never share a `panel.db`. Next runs one `next dev` per `frontend/`: `--port` beside a dev stack of
the same checkout fails to start its front end; run it from another checkout (a worktree). The concierge and banking bundles come from the platform's origin, so
they are nobody's peer here.

The first run builds the conductor, sites_panel and their front ends with Nix (minutes), and
each landing's `nix run .#dev` installs its `node_modules` (minutes again). Later runs start in
seconds.

What it does (`scripts/local-stack.sh`, which the flake app runs as is):

1. Keeps its state in `.local/` (ignored by its own `.gitignore`): `panel.db` and
   `sites_panel.db`, the `data-key` both share, the sources' secrets in `secrets/`, each
   landing's `<site>-leads.db`, review_archive's data dir in `review_archive/`, and the logs in
   `logs/`. It persists between runs; `--reset` wipes it. A fresh `panel.db` imports an empty
   document from review_archive, which opens tokens and GMBs.
2. Seeds, idempotently: the sources `aquafix-local` (brand `aquafix`, origin
   `<aquafix>`) and `vifnet-local` (brand `vifnet`, origin `<vifnet>`)
   in the conductor, each secret kept in `.local/secrets/` and reused, a key from an older run
   given its origin; the
   places `aquafix/{royat, clermont-ferrand, desgenettes, lyon-est, la-mouche, lyon-nord}` and
   `vifnet/vifnet` in sites_panel, registered with nothing set (`sites_panel place register`).
3. Starts `sites_panel serve` (its bundle from `PANEL_MFE_DIR`, the conductor's public key in
   `PANEL_ASSERTION_KEYS`), `review_archive serve` likewise (a headless Chromium on a signed-out
   profile: its Maps walks fail and say so), then `panel serve` in development, with dev sign-in, a signing key for this machine alone, `PANEL_SITES_URL`
   at sites_panel, and the front end's static export served on the same origin.
4. Starts each landing with its own `nix run .#dev` (kitstart's `next dev`), with:

   | Variable | Value |
   | --- | --- |
   | `PORT` | its dev shell's |
   | `LEAD_WEBHOOK_URL` | `<conductor>/api/ingest/v1/events` |
   | `LEAD_WEBHOOK_KEY_ID` / `LEAD_WEBHOOK_SECRET` | the seeded source's |
   | `SITES_PANEL_INTERNAL_URL` | `<sites_panel>/api/internal` |
   | `LEADS_DB_PATH` | `.local/<site>-leads.db` |
   | `POSTHOG_KEY` | unset: analytics off |

5. Prints the URLs, prefixes every log line with whose it is, and stops everything on Ctrl-C
   (or when the conductor, sites_panel or review_archive stops). Started from a non-interactive shell (an agent's, in the
   background), SIGINT does not reach it: stop it with `kill <pid>` (SIGTERM), which does the
   same. Each part runs in a process group of its own, so the caller's shell is never hit.

Iterating on the backend: `cargo build -p sa_server`, then
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
| Conductor | `<conductor>` — opens signed in |
| Leads, locations | `<conductor>/sites/leads`, `<conductor>/sites/places` |
| Sources, connected sites | `<conductor>/sources` (with `--role admin`) |
| sites_panel | `<sites_panel>` — `/api/internal` only without an assertion |
| review_archive, drawn by Grafana | `<conductor>/review_archive` (`?grafana=false`: its legacy UI, for that tab) |
| Grafana | `<conductor>/grafana/` |
| aquafix, a point | `<aquafix>` with the point's slug before `localhost`, e.g. `http://royat.localhost:<port>/fr` |
| aquafix, the brand page | `<aquafix>/fr` |
| vifnet | `<vifnet>/fr` |

`*.localhost` resolves to this machine in every current browser and in curl; nothing to add
to `/etc/hosts`.

## Dev sign-in

`PANEL_DEV_SIGN_IN` — an alias (`sa:admin`, `sa:operator`), a comma-separated list of `sa`
permissions, or `none` — with `PANEL_DEV_SIGN_IN_EMAIL` replaces concierge: `/auth/login` sends
the browser straight to `/auth/callback`, which opens a real session for that email's user
holding those permissions: the database's user of that email when it has one (a copy of prod's),
else one named by it. The email is the same whatever the permissions, so is the user and what
they hold; the stack's is `DEV_EMAIL` in `scripts/local-stack.sh`, prod's `admin@evinvest.ltd`.
An unknown permission fails the boot, named. The account menu shows the user as **Dev sign-in
(<email>)**, `GET /api/v1/me` answers
`"dev_sign_in": true` (for a UI badge), and `serve` logs `DEV SIGN-IN ON` at start.

The binary refuses it at start (exit 78, every command) in any `APP_ENV` but `development`,
beside any concierge variable, and unless `PANEL_PUBLIC_ORIGIN` is `http://localhost[:port]`
or `http://127.0.0.1[:port]` — so it cannot be switched on in the image, which sets
`APP_ENV=production`. `cargo test -p sa_server --test dev_sign_in` holds the binary to it.

Without the script, the conductor alone (no forward, so no ingest and no sites), in the dev shell,
which sets `PANEL_BIND`:

```sh
APP_ENV=development PANEL_DB_PATH=./panel.db PANEL_DATA_KEY="$(panel gen-data-key)" \
PANEL_PUBLIC_ORIGIN="http://$PANEL_BIND" PANEL_DEV_SIGN_IN=sa:admin \
PANEL_WEB_DIR="$(nix build .#frontend --print-out-paths)" panel serve
```

The front end's dev server instead of the export is `--watch`. By hand:
`cd frontend && PANEL_DEV_BACKEND=<conductor> npm run dev` serves on the dev shell's
`PANEL_WEB_PORT`; the sign-in's redirect goes to `PANEL_PUBLIC_ORIGIN`, so run the conductor as
above with `PANEL_PUBLIC_ORIGIN=http://localhost:$PANEL_WEB_PORT`. `npm run dev:stub` in
`frontend/` stands in for the backend instead.

A session in curl (what a browser does):

```sh
jar=$(mktemp)
curl -s -c "$jar" -b "$jar" -L -o /dev/null <conductor>/auth/login
curl -s -b "$jar" <conductor>/api/v1/me
# sites' API through the forward; a write needs the CSRF cookie echoed in x-sa-csrf, and
# expected_updated_at is the place's current updated_at (GET …/settings), null for one never set
csrf=$(awk '$6 == "sa_csrf" { print $7 }' "$jar")
curl -s -b "$jar" -H "x-sa-csrf: $csrf" -H 'content-type: application/json' \
  -X PUT <conductor>/api/sites/places/aquafix/royat/settings \
  -d '{"settings": {"phone": "+33499887766"}, "expected_updated_at": null}'
```

Or skip the session: `sites_panel place set aquafix royat --phone +33499887766` with the stack's
`PANEL_DB_PATH=.local/sites_panel.db PANEL_DATA_KEY=$(cat .local/data-key)`.

## Prod's data

`--from-prod` (with `--reset`: it fills a state that has none of them) restores prod's `panel.db`,
`sites_panel.db` and `review_archive.db` from their litestream replicas in R2, as gitops main
configures them (`../gitops`, fetched), with the R2 keys from `../devops`'s sops
(`DEVOPS_DIR`, `GITOPS_DIR` name others), and copies review_archive's blobs, which are not
replicated, off its volume over ssh (`PROD_SSH` in the script). Everyone's data comes along; dev
sign-in is prod's `admin@evinvest.ltd`. What prod sealed under its own `PANEL_DATA_KEY` (prod's
sessions, its sources' secrets, sites' Google tokens) does not open here: the landings sign with
the stack's own `<brand>-local` keys. Nothing walks Maps, nothing is told to Telegram. Prod's
GMBs come without the cache that located them: `panel gmb locate` (the stack's `PANEL_DB_PATH`
and data key) asks for their points once.

## Google

The conductor names, addresses and locates GMBs with `gmaps_optimal_placement`'s keys, in the
order `~/.config/gmaps_optimal_placement.nix` lists them, and the map loads with the first:

| Variable | Value, unless the shell sets it |
| --- | --- |
| `GOOGLE_MAPS_KEYS` | `$GOOGLE_MAPS_KEY,$GOOGLE_MAPS_KEY_ICOS1` |
| `GOOGLE_MAPS_BROWSER_KEY` | `$GOOGLE_MAPS_KEY` |

Without `GOOGLE_MAPS_KEY` in the shell, a GMB is added by its place id alone and `/` has no map.
Answers are kept in `.local/google_cache/`: `--reset` pays for them again.

## The end-to-end check

With the stack up:

**A lead, landing → sites**

1. Open royat's point (`http://royat.localhost:<aquafix's port>/fr`) and submit the quote form (any valid French mobile,
   e.g. `0612345678`, and a postcode such as `63130`). The site answers with its thanks page.
2. The site keeps the lead in `.local/aquafix-leads.db` and its outbox posts it, signed, to
   the conductor within about 10 s (kitstart's `WEBHOOK_TICK_MS`), which forwards it to
   sites_panel: `[sites]` logs `ingested a batch key_id="aquafix-local" accepted=1`; the site's
   side is the row in
   `sqlite3 .local/aquafix-leads.db 'select id, state, attempts, last_error from webhook_outbox'`
   (`delivered`).
3. Open `<conductor>/sites/leads`: the lead is there, stage *created*, brand aquafix,
   location royat, with what you typed — it came in live.

The same for vifnet at `<vifnet>/fr`. Without a browser, the form's no-JS path:

```sh
t=$(( $(date +%s) * 1000 - 5000 ))   # rendered five seconds ago, as a person would have
curl -si http://royat.localhost:<aquafix's port>/quote --data-urlencode locale=fr --data-urlencode form_id=quote \
  --data-urlencode t=$t --data-urlencode location=royat --data-urlencode job=blocked_drain \
  --data-urlencode zip=63130 --data-urlencode mobile=0612345678 | head -1      # 303 → /fr/thanks
t=$(( $(date +%s) * 1000 - 5000 ))
curl -si <vifnet>/quote --data-urlencode locale=fr --data-urlencode form_id=quote \
  --data-urlencode t=$t --data-urlencode location=vifnet --data-urlencode subject=deep \
  --data-urlencode locality=83702 --data-urlencode mobile=2085550192 \
  --data-urlencode 'name=Smoke Test' --data-urlencode bedrooms=3 | head -1
```

**A board, review_archive → Grafana**: `SA_CONDUCTOR_ORIGIN=<conductor> node examples/with_grafana.cjs` in review_archive's
checkout drives a headless browser through the overview and a place's board (its header says
what it checks).

**A place, sites → landing**

1. Open `<conductor>/sites/places`, pick aquafix / royat, set a phone (e.g.
   `+33 4 23 50 06 40`) and save.
2. `curl -s <sites_panel>/api/internal/brands/aquafix/locations/royat` answers the
   new phone at once: that is what the site reads.
3. Reload royat's point: the new number shows — see the cache note below.

**Connected sites, landing → conductor**

1. `curl -s <aquafix>/.well-known/sa-site` answers aquafix's own report: the state of
   each place, of pricing and of experiments as it reads them from sites_panel (`live`, `empty`,
   `withdrawn`; `down` with why, `unset` without `SITES_PANEL_INTERNAL_URL`).
2. Open `<conductor>/sources`: the Connected sites table shows both keys with that
   report, kitstart's version, the last event sites took from each key and the last time each
   brand's data was read from sites' `/api/internal`.

**The cache.** kitstart fetches a place with `cache: "force-cache"` and a 600 s revalidate
(`PLACE_REVALIDATE_SECONDS`), and the point pages are ISR with `revalidate = 600`. In
production a change therefore shows within about 10 minutes (the first request after the
TTL re-renders in the background, the next one gets it). Under `next dev` the fetch is cached
the same way (in memory, 600 s): a plain reload keeps the old number. To see a change at once:

- a **hard reload** (Cmd-Shift-R / Ctrl-Shift-R): the browser sends `Cache-Control: no-cache`,
  and `next dev` then fetches afresh — later plain reloads get the new value too. In curl:
  `curl -H 'cache-control: no-cache' http://royat.localhost:<aquafix's port>/fr`;
- or restart the stack (the cache is in memory).

Production has no such bypass: there, wait out the 600 s.

## Troubleshooting

- **`port … is taken`**: another stack's part, or something else —
  `lsof -nP -iTCP:<port> -sTCP:LISTEN` names it.
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
