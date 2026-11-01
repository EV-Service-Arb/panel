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
