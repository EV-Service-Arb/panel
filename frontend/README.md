# Panel front end

Next.js (App Router), Feature-Sliced Design, `@evinvest/uikit`. The screens are the
ones of `SA-PANEL-SPEC.md` §10 (hybrid 1+2): Overview, Leads, Locations, Experiments,
Sources (admin), and on a phone a bottom tab bar with "More" in place of the sidebar. The
rail's foot holds what is the caller's own: the wallet and Usage (access, tokens, spending).

```text
app/                 routes: (panel)/ is everything behind sign-in, access/ is not
src/views/           one slice per screen, plus shell/ (the kit's AppShell: rail, tab bar, nav marks, access states)
src/features/        call-lead · move-stage · record-payment · create-lead ·
                     lead-filters · funnel-filters · manage-sources · sign-out ·
                     edit-place-settings · revert-place-change · withdraw-place · add-place ·
                     configure-experiment · request-access
src/entities/        session · lead · funnel · experiment · place · source — types, response checks, requests
src/shared/          api/ (fetch, CSRF, the gate's answers), i18n/, lib/ (live/: the socket), ui/
messages/            en.json (the source of keys) and ru.json
scripts/dev-stub.ts  a stand-in backend for local work (place settings in scripts/stub-places.ts,
                     experiments in scripts/stub-experiments.ts)
tests/               vitest, in Node: the rules the screens obey live in plain modules
```

## Running it

```sh
npm ci
npm run dev:stub     # :3121 — the operator API over made-up leads; STUB_ROLE=admin, STUB_ME=401|403|503,
                     # STUB_MIN_SAMPLE=2 for percents on so few leads, STUB_POSTHOG=off for no PostHog links,
                     # STUB_PLACES_CONFLICT=1 for a 409 on every place-settings save and revert,
                     # STUB_LIVE=off for no socket (the panel polls), STUB_LIVE_EVERY=5 for busier live activity,
                     # STUB_BOOKING_CONFLICT=1 for a 409 on every booking write
npm run dev          # :3120 — proxies /api and /auth to PANEL_DEV_BACKEND (default the stub)
```

Against the real backend, point `PANEL_DEV_BACKEND` at `panel serve` with sign-in
configured and `PANEL_PUBLIC_ORIGIN=http://localhost:3120`, so the callback lands here.

The top bar's account chip and the sign-in dialog load from the platform origin
(`evinvest.ltd`), named by its published registry. If the registry is unreachable, the
chip's fallback shows. Who the account is lives on the platform's account page, the chip's
header row; the panel adds only its own entries (token history, view as member).

Checks: `npm run typecheck`, `npm run lint`, `npm test`, `npm run build`.

## How it is served

`next build` is a static export (`out/`): every screen reads `/api/v1` from the
browser with the session cookie, so a Node server would have nothing to render,
and the session cookie is `HttpOnly` anyway. The Rust binary serves `out/` on the
same origin as `/api` and `/auth` — one image, one Service, one IngressRoute, as
aquafix has one. What that needs of the backend and the flake:

- `panel serve` gets a fallback: files from a directory (say `PANEL_WEB_DIR`),
  a directory answering with its `index.html` (the export writes `leads/index.html`
  and links `/leads/`), and `404.html` with status 404 for anything else. `/api`,
  `/auth`, `/health` and later `/grafana` keep their routes ahead of it.
- Cache headers: `/_next/static/*` is content-hashed — `public, max-age=31536000,
  immutable`; every HTML file `no-cache`. A CSP (`default-src 'self'`; Next's inline
  bootstrap and payload run by their sha256, hashed from the export at boot: no
  `'unsafe-inline'` in `script-src`). An inline `<script>` added at runtime is refused.
- The flake builds `frontend/` with `buildNpmPackage` (needs `npmDepsHash`) and puts
  `out/` in the container beside the binary; the entrypoint sets the directory.
- The IngressRoute for `sa.evinvest.ltd` sends everything to the panel's Service
  except `/api/ingest` (in-cluster only, `docs/ARCHITECTURE.md`).

## Live updates

The shell opens one WebSocket per tab, same origin, `/api/v1/live` (cookie
session). The server says `changed` with a topic after each commit and `resync`
when the client should re-read everything; it closes with 4401 (session gone:
the browser goes to sign-in) or 4403 (access lost: the "no access" screen).
`shared/lib/live` reconnects with exponential backoff and jitter, re-reads every
mounted screen after any reconnect, and falls back to a re-read every 60 s when
the socket cannot be had (three attempts in a row that never opened). A hidden
tab keeps an open socket but makes no attempts; visible again, it reconnects at
once.

Reads follow topics through `useResource(…, { live })`, debounced (a batch
import sends one `changed` per event). What the screens do with it:

- **Leads**: arrivals wait in a "New leads: N — Show" banner rather than moving
  the rows; rows on screen update in place and glow; a row that left the filter
  is re-read and kept. The open card follows its lead and notes a change made
  elsewhere (the event names no person, so the note says when and what).
- **Locations**: an open form is never overwritten — with no edits it takes the
  newer data, with edits it says who saved and offers to load it (saving would
  be refused with 409 anyway).
- **Nav**: Leads counts new leads since the screen was last in front of you (the
  baseline kept in localStorage per person), Locations the places changed,
  Experiments and Sources a dot; opening the screen clears it. While the tab is
  not focused its title counts new leads: `(3) Leads — Service-Arb panel`.

## Place settings

A location card opens its live site data (`SA-PANEL-PLACE-SETTINGS-SPEC.md`):
phone, WhatsApp, opening hours and service area, which the site merges over its
baked config within its 600 s fetch TTL. The form edits only those four; any
other live field (address, geo, photo, landmark, rating) goes back as it came,
since a save is a full replace guarded by `expected_updated_at` (409 → reload).
A 422 names its fields and the form shows each reason on its field. The history
reads each change as before → after, with a revert guarded the same way; an admin can withdraw a
point (the site then answers 404 for it) and add one by hand. An operator reads.

The preview of the contact channels assumes the places' hours are Paris time
(`entities/place/config/time-zone.ts`): the live data carries no zone.

## What the API does not answer yet

- **Stages 1–2** (Maps) arrive with the GBP import; until then the overview says so
  rather than showing zeros.
- **Stages 3–4** (site visits, contact intents) and the experiments' results are
  PostHog's and are read there; the panel does not count them again. The
  Experiments screen is config: what each landing declared, the split it runs,
  and for an admin the kill switch, the weights and the holdout, with a link to
  the experiment's funnel in PostHog when the backend has a project configured.
- **Lost reasons** are a slug the backend takes freely; the panel offers a fixed
  list (`features/move-stage/model/moves.ts`) so the reports can group by it.
