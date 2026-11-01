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
