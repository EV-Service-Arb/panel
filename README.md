# panel
![Minimum Supported Rust Version](https://img.shields.io/badge/nightly-1.100+-ab6000.svg)
![Lines Of Code](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/valeratrades/b48e6f02c61942200e7d1e3eeabf9bcb/raw/panel-loc.json)
<br>
[<img alt="ci errors" src="https://img.shields.io/github/actions/workflow/status/EV-Service-Arb/site_conductor/errors.yml?branch=main&style=for-the-badge&style=flat-square&label=errors&labelColor=420d09" height="20">](https://github.com/EV-Service-Arb/site_conductor/actions?query=branch%3Amain) <!--NB: Won't find it if repo is private-->
[<img alt="ci warnings" src="https://img.shields.io/github/actions/workflow/status/EV-Service-Arb/site_conductor/warnings.yml?branch=main&style=for-the-badge&style=flat-square&label=warnings&labelColor=d16002" height="20">](https://github.com/EV-Service-Arb/site_conductor/actions?query=branch%3Amain) <!--NB: Won't find it if repo is private-->

The Service-Arb conductor (`sa.evinvest.ltd`): the host the Service-Arb services live behind.
It knows who a person is — people sign in through concierge (the conductor is its relying
party, client `sa`, and publishes the whole `sa` permission catalog) — and who a machine is: the
source keys the landings, bots and other sources sign what they send with.

Everything else is a service behind the origin, told who is calling by an assertion the
conductor signs for that one request: sites_panel (the sites business — leads, places, pricing,
experiments, bookings, invoices — under `/api/sites` and the `/sites` page), review_archive and
playbook. Ingest is authenticated here and forwarded to sites with the key's grant; what a key
may write is sites' to judge.

Two hubs run here too: live updates (one WebSocket per screen, fed by sites' change feed and the
conductor's own changes, each told only to who may read it) and a Telegram bot (each user's
private chat, linked from their profile; sites' notes and the conductor's own — a source gone
silent, an access request — through an outbox paced to Telegram's limits; buttons that call
sites as the user who pressed).

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for where things live.
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
# by whichever command opens it first), and PANEL_DATA_KEY (64 hex characters) that seals the
# sources' HMAC secrets, the sessions' tokens and the queued Telegram texts.
# `panel --print-required-vars` lists what production needs.
export PANEL_DATA_KEY="$(panel gen-data-key)"
export PANEL_DB_PATH=./panel.db

# Every command migrates on open; this one does nothing else.
panel migrate

# A source: its key signs what it sends for the brands named. The secret is printed once.
panel source add aquafix-site --kind site --brand aquafix
panel source add aquafix-tg --kind bot --brand aquafix      # a messenger bot
panel source list
panel source revoke aquafix-site

# HTTP on 127.0.0.1:59120, with signing in — all four, or PANEL_DEV_SIGN_IN in development:
#   PANEL_PUBLIC_ORIGIN=https://sa.evinvest.ltd   CONCIERGE_PUBLIC_ORIGIN=https://evinvest.ltd
#   CONCIERGE_GRPC_ADDR=http://concierge:55670    RP_CLIENT_SECRET_SA=<the secret concierge hashed>
# and the forward — all four or none: PANEL_ASSERTION_KEY (`cargo r -p sa_auth --example keygen`),
#   PANEL_SITES_URL, PANEL_REVIEW_ARCHIVE_URL, PANEL_PLAYBOOK_URL
# TELEGRAM_BOT_TOKEN turns the bot on (it needs the forward; TELEGRAM_BOT_USERNAME spares a
# getMe, TELEGRAM_LOCALE=ru|en picks its language, ru by default).
panel serve
```

## Sending events

```text
POST /api/ingest/v1/events
x-sa-key-id:    aquafix-site
x-sa-timestamp: 1790762400                       unix seconds; ±5 minutes of the conductor's clock
x-sa-signature: hex(HMAC-SHA256(secret, "sa-ingest/v1." + <x-sa-timestamp> + "." + <raw body>))

{"events": [ …1 to 500 sa.v1.Event, protojson… ]}
```

The conductor checks the window, the key and the signature (`401` refuses the request) and
hands the body to sites_panel, whose answer comes back as it is: `207` with a verdict per
event, and the rest of its contract (sites_panel's docs). A messenger bot (a key of kind `bot`)
looks a lead up by the ref the customer brought the same way:
`GET /api/ingest/v1/leads/by-ref/<brand>/<ref>`, signed over `GET <path>` in place of a body.

## Running it locally

```sh
nix run .#local-stack    # the conductor on :59120 (signed in as a dev admin), sites_panel on :59121,
                         # and the aquafix and vifnet landings from the checkouts beside it, wired
                         # together; Ctrl-C stops all
```

A lead posted on a local landing shows on the local sites page, a phone edited there shows on
the landing. `PANEL_DEV_SIGN_IN=sa:admin|sa:operator|<permissions>|none` stands in for
concierge, in development on loopback only. The prerequisites, the end-to-end check and
troubleshooting are in [docs/LOCAL.md](docs/LOCAL.md).

## Tests

`cargo test` runs everything, here and in CI: each database test gets its own throwaway SQLite
file in the temp directory (`panel_test_*.db`), removed when it ends; the services behind the
conductor, concierge and Telegram are stand-ins on loopback ports. Nothing to set up.


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

