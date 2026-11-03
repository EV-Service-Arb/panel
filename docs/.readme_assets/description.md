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
conductor's own changes, each told only to who may read it) and alerts (sites' and
review_archive's notes and the conductor's own — a lead, a review gone, a source silent, an
access request — to each user's Telegram, and the channels and groups they run, through an
outbox paced to Telegram's limits; buttons that call sites as the user who pressed).

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for where things live.
