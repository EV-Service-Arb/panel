# panel
![Minimum Supported Rust Version](https://img.shields.io/badge/nightly-1.100+-ab6000.svg)
![Lines Of Code](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/valeratrades/b48e6f02c61942200e7d1e3eeabf9bcb/raw/panel-loc.json)
<br>
[<img alt="ci errors" src="https://img.shields.io/github/actions/workflow/status/Service-Arb/panel/errors.yml?branch=main&style=for-the-badge&style=flat-square&label=errors&labelColor=420d09" height="20">](https://github.com/Service-Arb/panel/actions?query=branch%3Amain) <!--NB: Won't find it if repo is private-->
[<img alt="ci warnings" src="https://img.shields.io/github/actions/workflow/status/Service-Arb/panel/warnings.yml?branch=main&style=for-the-badge&style=flat-square&label=warnings&labelColor=d16002" height="20">](https://github.com/Service-Arb/panel/actions?query=branch%3Amain) <!--NB: Won't find it if repo is private-->

The Service-Arb panel (`sa.evinvest.ltd`): where the funnel of the Service-Arb brands — from a
lead to a payment — is recorded and read. The plan is `SA-PANEL-SPEC.md` (§ references in the
code point there); this repository holds its backend.

What exists so far is ingest. Sources (the landings, and later review_archive and a GBP
import, the panel's own screens) send `sa.funnel.v1` events — a versioned protobuf
contract, spoken as protojson — to `POST /api/ingest/v1/events`, signed with a per-source HMAC
key that may write only for its own brands. Every accepted event goes into an append-only
journal in SQLite (one file, replicated off the pod by litestream), PII sealed apart; the
funnel's projections (`leads` with their stages, `calls`, `payments`) are derived from it and
can be rebuilt from it at any time. The `reporting_*` views expose them without PII.

People sign in through concierge (the panel is its relying party, client `sa`): any active
account signs in, and the `sa` permissions it holds (the panel publishes their catalog; the
aliases `sa:operator` and `sa:admin` bundle them) open its sections. `/api/v1` is the
operator API the panel's front end works through — leads and their stages, semi-manual
calls, payments typed in by hand, the funnel, and (admins) the sources.

A Telegram bot notifies each user in a private chat they link from their profile — a new
lead, with buttons that record "taken" and "no answer" as the operator API would; a lead past
its contact SLA; for admins, payments and a source gone silent — through an outbox in the
same database, paced to Telegram's limits.

Admins edit each place's live settings — the phones, hours and service area the landings
show — and each brand's price list and experiments (a kill switch, the weights, the holdout
over what the landing declares), and the landings read them from the panel instead of a
release. What PostHog counts (visits, intents, the experiments' numbers) is looked at in
PostHog; the panel sends it each lead's life after the form, without PII.

Not here yet: the GBP import. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for where things live.
<!-- markdownlint-disable -->
<details>
<summary>
<h2>Installation</h2>
</summary>

nix build

</details>
<!-- markdownlint-restore -->

## Usage
```sh
# Settings come from the environment only: PANEL_DB_PATH, the SQLite file (created and migrated
# by whichever command opens it first), and PANEL_DATA_KEY (64 hex characters) that seals PII
# and the sources' HMAC secrets. `panel --print-required-vars` lists what production needs.
export PANEL_DATA_KEY="$(panel gen-data-key)"
export PANEL_DB_PATH=./panel.db

# Every command migrates on open; this one does nothing else.
panel migrate

# A source: its key may write events of one kind, for the brands named. The secret is printed once.
panel source add aquafix-site --kind site --brand aquafix
panel source add aquafix-tg --kind bot --brand aquafix      # a messenger bot: docs/BOT-API.md
panel source list
panel source revoke aquafix-site

# HTTP on 127.0.0.1:59120. Ingest alone, unless signing in is configured — all four or none:
#   PANEL_PUBLIC_ORIGIN=https://sa.evinvest.ltd   CONCIERGE_PUBLIC_ORIGIN=https://evinvest.ltd
#   CONCIERGE_GRPC_ADDR=http://concierge:55670    RP_CLIENT_SECRET_SA=<the secret concierge hashed>
# which adds /auth/login, /auth/callback, /auth/logout and the operator API under /api/v1.
# With signing in, TELEGRAM_BOT_TOKEN turns the bot on (long polling; TELEGRAM_BOT_USERNAME
# spares a getMe, TELEGRAM_LOCALE=ru|en picks its language, ru by default).
panel serve

# leads, calls and payments again from the journal, against the registry as it is now
panel rebuild-projections

# A place's live settings (see "Place settings"): set some fields, clear others, the rest stay
panel place register aquafix royat             # known to the panel, nothing set; a no-op when known
panel place set aquafix royat --phone +33423500640 --whatsapp +33612345678 \
  --hours 'Mo-Fr 08:00-19:00,Sa 09:00-12:00' --service-area 'Royat,Chamalières'
panel place set aquafix royat --clear whatsapp
panel place set aquafix royat --telegram aquafix_devis_bot --messengers whatsapp=on,telegram=off
panel place set aquafix royat --clear telegram --clear messengers
panel place show aquafix royat
panel place history aquafix royat              # every change, newest first, with its id
panel place revert aquafix royat <change id>   # the settings that change found, back
panel place withdraw aquafix royat             # the sites answer it as gone (404)
panel place restore aquafix royat
```

## Sending events

```text
POST /api/ingest/v1/events
x-sa-key-id:    aquafix-site
x-sa-timestamp: 1790762400                       unix seconds; ±5 minutes of the panel's clock
x-sa-signature: hex(HMAC-SHA256(secret, "sa-ingest/v1." + <x-sa-timestamp> + "." + <raw body>))

{"events": [ …1 to 500 sa.v1.Event, protojson… ]}
```

The answer is `207` with a verdict per event, in order: `accepted`, `duplicate` (that id is
journaled already), or `rejected` with a reason. A type the panel does not know yet is
`accepted` and stored, and projected once it is registered. `401` refuses the whole batch
(key, signature or timestamp), `400` a body that is not a batch. The contract is
[`contracts/proto/sa/v1/events.proto`](contracts/proto/sa/v1/events.proto).

A messenger bot (a key of kind `bot`) sends its leads and the fact that a customer wrote the
same way, and looks a lead up by the ref the customer brought:
`GET /api/ingest/v1/leads/by-ref/<brand>/<ref>`, signed over `GET <path>` in place of a body. See
[docs/BOT-API.md](docs/BOT-API.md).

## Place settings

A landing (kitstart) bakes its places into its build, and lays over them what the panel
answers for each one: phones, WhatsApp, the Telegram bot and the messenger switches, opening
hours, service area, and for storefronts an address, a pin, a photo, a landmark, a rating
(kitstart's `PlaceLive`). Changing a number is
an edit in the panel or a `panel place set`, not a release. A site fetches at most every
10 minutes; the panel being down or slow only delays a change, the site serves what it baked.

```text
GET /api/internal/brands/<brand>/locations/<slug>?locale=fr
    200 {"phone": "+33…", "hours": [{"days": ["Monday"], "opens": "08:00", "closes": "19:00"}], …}
        only the fields set; {} for a place without settings or unknown to the panel
    404 {"error": "not_found"}  only for a place an admin withdrew: the site 404s that page
```

A site points at it with, in its deploy config (in-cluster, not a secret):

```sh
LOCATIONS_API_URL=http://panel.service-arb.svc.cluster.local:59120/api/internal/brands/<brand>
```

`/api/internal` has no session and is not published: the public IngressRoute must exclude it,
and the NetworkPolicy lets the landings' pods in. Admins edit in the panel (operators read);
every change, from the panel or the CLI (`by = cli`), is journaled with what was before and
after, and can be reverted. The settings are checked as kitstart reads them, refused field
by field (`422`) where kitstart would quietly drop them. The session API is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#place-settings).

## Running it locally

```sh
nix run .#local-stack    # the panel on :59120 (signed in as a dev admin) and the aquafix and vifnet
                         # landings from the checkouts beside it, wired together; Ctrl-C stops all
```

A lead posted on a local site lands in the local panel, a phone edited in the panel shows on
the site. `PANEL_DEV_SIGN_IN=sa:admin|sa:operator|<permissions>|none` stands in for concierge, in development on
loopback only. The prerequisites, the end-to-end check and troubleshooting are in
[docs/LOCAL.md](docs/LOCAL.md).

## Tests

`cargo test` runs everything, here and in CI: each database test gets its own throwaway SQLite
file in the temp directory (`panel_test_*.db`), removed when it ends. Nothing to set up.


<br>

<sup>
	This repository follows <a href="https://github.com/valeratrades/.github/tree/master/best_practices">my best practices</a> and <a href="https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md">Tiger Style</a> (except "proper capitalization for acronyms": (VsrState, not VSRState) and formatting). For project's architecture, see <a href="./docs/ARCHITECTURE.md">ARCHITECTURE.md</a>.
</sup>

#### License

<sup>
	Licensed under <a href="LICENSE">Blue Oak 1.0.0</a>
</sup>

<br>

<sub>
	Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be licensed as above, without any additional terms or conditions.
</sub>

