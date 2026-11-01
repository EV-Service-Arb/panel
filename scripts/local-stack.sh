#!/usr/bin/env bash
# The conductor (its Grafana included), sites_panel, review_archive and both landings on this
# machine, wired together: a lead posted on a landing lands in sites_panel through the conductor's
# ingest, a place edited on the sites page shows on the landing, review_archive's boards draw in
# the conductor's Grafana. See docs/LOCAL.md.
#
#   nix run .#local-stack                  build, seed, start everything; Ctrl-C stops it all
#   nix run .#local-stack -- --reset       wipe .local/ (databases, keys, the landings' leads) first
#   nix run .#local-stack -- --no-sites    the conductor and sites_panel, no landings
#   nix run .#local-stack -- --role operator
#
# The conductor and its front end come from `nix build .#bin` / `.#frontend` of this checkout,
# unless PANEL_BIN / PANEL_WEB_DIR name others (PANEL_BIN=target/debug/panel for a cargo build);
# sites_panel and its bundle from its checkout's, unless SITES_BIN / SITES_MFE_DIR do; review_archive
# likewise (REVIEW_ARCHIVE_BIN / REVIEW_ARCHIVE_MFE_DIR), and Grafana's server (PANEL_GRAFANA_SERVER).
# SITES_DIR, REVIEW_ARCHIVE_DIR, AQUAFIX_DIR and VIFNET_DIR name the checkouts, by default beside
# this one's.
set -euo pipefail

PANEL_PORT=59120
SITES_PORT=59121
REVIEW_ARCHIVE_PORT=59110
AQUAFIX_PORT=59081
VIFNET_PORT=59082
AQUAFIX_PLACES=(royat clermont-ferrand desgenettes lyon-est la-mouche lyon-nord)
VIFNET_PLACES=(vifnet)

reset=0
sites=1
role="admin"
while [ $# -gt 0 ]; do
	case "$1" in
	--reset) reset=1 ;;
	--no-sites) sites=0 ;;
	--role)
		role="${2:-}"
		shift
		;;
	-h | --help)
		cat <<'EOF'
local-stack [--reset] [--no-sites] [--role admin|operator]

  The conductor on http://127.0.0.1:59120 (dev sign-in, its Grafana at /grafana/), sites_panel
  on :59121, review_archive on :59110, aquafix on :59081 and vifnet on :59082, wired together. State in .local/ (--reset wipes it). Ctrl-C stops everything. See docs/LOCAL.md.
EOF
		exit 0
		;;
	*)
		echo "unknown argument: $1 (see --help)" >&2
		exit 2
		;;
	esac
	shift
done
case "$role" in
admin | operator) ;;
*)
	echo "--role is admin or operator, not '$role'" >&2
	exit 2
	;;
esac

say() { printf '\033[1m▶ %s\033[0m\n' "$*" >&2; }
die() {
	printf '\033[31m✗ %s\033[0m\n' "$*" >&2
	exit 1
}

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "run this from inside the conductor's checkout"
[ -f "$root/crates/panel_server/Cargo.toml" ] || die "$root is not the conductor's checkout"
# The others sit beside the conductor's main checkout (_sa/{site_conductor,sites/panel,aquafix,
# vifnet}), also when this is one of its worktrees.
main_checkout="$(dirname "$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)")"
service_arb="$(dirname "$main_checkout")"
SITES_DIR="${SITES_DIR:-$service_arb/sites/panel}"
REVIEW_ARCHIVE_DIR="${REVIEW_ARCHIVE_DIR:-$service_arb/review_archive}"
AQUAFIX_DIR="${AQUAFIX_DIR:-$service_arb/aquafix}"
VIFNET_DIR="${VIFNET_DIR:-$service_arb/vifnet}"

state="${LOCAL_STACK_DIR:-$root/.local}"
if [ "$reset" = 1 ] && [ -d "$state" ]; then
	say "--reset: wiping $state"
	rm -rf "$state"
fi
mkdir -p "$state/logs" "$state/secrets"
# Ignored by its own .gitignore: the repo's is generated, and this is nobody else's business.
printf '*\n' >"$state/.gitignore"

# ── ports ────────────────────────────────────────────────────────────────────────────────

busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
ports=("$PANEL_PORT" "$SITES_PORT" "$REVIEW_ARCHIVE_PORT")
[ "$sites" = 1 ] && ports+=("$AQUAFIX_PORT" "$VIFNET_PORT")
for port in "${ports[@]}"; do
	busy "$port" && die "port $port is taken — another local stack, or \`lsof -nP -iTCP:$port -sTCP:LISTEN\` says who"
done

# ── the builds ───────────────────────────────────────────────────────────────────────────

if [ -z "${PANEL_BIN:-}" ]; then
	say "building the conductor (nix build .#bin)"
	nix build "$root#bin" --out-link "$state/build/bin"
	PANEL_BIN="$state/build/bin/bin/panel"
fi
if [ -z "${PANEL_WEB_DIR:-}" ]; then
	say "building the front end (nix build .#frontend)"
	nix build "$root#frontend" --out-link "$state/build/frontend"
	PANEL_WEB_DIR="$state/build/frontend"
fi
[ -x "$PANEL_BIN" ] || die "PANEL_BIN $PANEL_BIN is not an executable"
if [ -z "${SITES_BIN:-}" ]; then
	[ -f "$SITES_DIR/flake.nix" ] || die "no sites_panel checkout at $SITES_DIR: SITES_DIR names it"
	say "building sites_panel ($SITES_DIR#bin)"
	nix build "$SITES_DIR#bin" --out-link "$state/build/sites"
	SITES_BIN="$state/build/sites/bin/sites_panel"
fi
if [ -z "${SITES_MFE_DIR:-}" ]; then
	say "building the sites' bundle ($SITES_DIR#frontend)"
	nix build "$SITES_DIR#frontend" --out-link "$state/build/sites-mfe"
	SITES_MFE_DIR="$state/build/sites-mfe"
fi
[ -x "$SITES_BIN" ] || die "SITES_BIN $SITES_BIN is not an executable"
if [ -z "${PANEL_GRAFANA_SERVER:-}" ]; then
	say "building Grafana (nix build .#grafana-server)"
	nix build "$root#grafana-server" --out-link "$state/build/grafana-server"
	PANEL_GRAFANA_SERVER="$state/build/grafana-server"
fi
review_archive=1
if [ -z "${REVIEW_ARCHIVE_BIN:-}" ] || [ -z "${REVIEW_ARCHIVE_MFE_DIR:-}" ]; then
	if [ -f "$REVIEW_ARCHIVE_DIR/flake.nix" ]; then
		if [ -z "${REVIEW_ARCHIVE_BIN:-}" ]; then
			say "building review_archive ($REVIEW_ARCHIVE_DIR#bin)"
			nix build "$REVIEW_ARCHIVE_DIR#bin" --out-link "$state/build/review_archive"
			REVIEW_ARCHIVE_BIN="$state/build/review_archive/bin/review_archive"
		fi
		if [ -z "${REVIEW_ARCHIVE_MFE_DIR:-}" ]; then
			say "building review_archive's bundle ($REVIEW_ARCHIVE_DIR#mfe)"
			nix build "$REVIEW_ARCHIVE_DIR#mfe" --out-link "$state/build/review_archive-mfe"
			REVIEW_ARCHIVE_MFE_DIR="$state/build/review_archive-mfe"
		fi
	else
		echo "skipping review_archive: no checkout at $REVIEW_ARCHIVE_DIR (REVIEW_ARCHIVE_DIR names it); /api/review_archive answers 502" >&2
		review_archive=0
	fi
fi

# ── the environments ─────────────────────────────────────────────────────────────────────

[ -s "$state/data-key" ] || "$PANEL_BIN" gen-data-key >"$state/data-key"
origin="http://127.0.0.1:$PANEL_PORT"
sites_origin="http://127.0.0.1:$SITES_PORT"
review_archive_origin="http://127.0.0.1:$REVIEW_ARCHIVE_PORT"
# A key for this machine alone: the 32 bytes 0..31, and its public half for sites_panel.
assertion_key="local:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
assertion_public="local:A6EHv/POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg="
# Whatever the calling shell exports for a deployed conductor must not leak in: concierge's
# variables would refuse the dev sign-in, a bot token would start the bot.
panel() {
	env -u CONCIERGE_PUBLIC_ORIGIN -u CONCIERGE_GRPC_ADDR -u RP_CLIENT_SECRET_SA \
		-u TELEGRAM_BOT_TOKEN -u SENTRY_DSN \
		APP_ENV=development \
		PANEL_DB_PATH="$state/panel.db" \
		PANEL_DATA_KEY="$(cat "$state/data-key")" \
		PANEL_PUBLIC_ORIGIN="$origin" \
		PANEL_DEV_SIGN_IN="sa:$role" \
		PANEL_WEB_DIR="$PANEL_WEB_DIR" \
		PANEL_ASSERTION_KEY="$assertion_key" \
		PANEL_SITES_URL="$sites_origin" \
		PANEL_REVIEW_ARCHIVE_URL="$review_archive_origin" \
		PANEL_GRAFANA_SERVER="$PANEL_GRAFANA_SERVER" \
		PANEL_GRAFANA_DIR="$state/grafana" \
		PANEL_PLAYBOOK_URL="http://127.0.0.1:9" \
		"$PANEL_BIN" "$@"
}
sites_panel() {
	env -u POSTHOG_PROJECT_API_KEY -u POSTHOG_PROJECT_ID -u SENTRY_DSN \
		APP_ENV=development \
		PANEL_DB_PATH="$state/sites_panel.db" \
		PANEL_DATA_KEY="$(cat "$state/data-key")" \
		PANEL_ASSERTION_KEYS="$assertion_public" \
		PANEL_MFE_DIR="$SITES_MFE_DIR" \
		"$SITES_BIN" "$@"
}

# review_archive on its data dir (REVIEW_ARCHIVE_DATA_DIR, e.g. a copy of its checkout's data/),
# trusting the conductor's assertions, HTTP only: the stack holds the dir's serve.lock, so nothing
# walks Maps and the boards draw what is stored.
REVIEW_ARCHIVE_DATA_DIR="${REVIEW_ARCHIVE_DATA_DIR:-$state/review_archive}"
review_archive() {
	mkdir -p "$state/review_archive" "$REVIEW_ARCHIVE_DATA_DIR"
	cat >"$state/review_archive/config.toml" <<EOF
data_dir = "$REVIEW_ARCHIVE_DATA_DIR"
mfe_dir = "$REVIEW_ARCHIVE_MFE_DIR"
bind = "127.0.0.1:$REVIEW_ARCHIVE_PORT"
[browser]
lean = []
[tokens]
start_cap_multiple = 2.0
EOF
	env -u SENTRY_DSN -u TELEGRAM_BOT_TOKEN -u GOOGLE_MAPS_KEY \
		APP_ENV=development \
		PANEL_ASSERTION_KEYS="$assertion_public" \
		"$REVIEW_ARCHIVE_BIN" --config "$state/review_archive/config.toml" "$@"
}

# ── seed: idempotent, so every run may do it ─────────────────────────────────────────────

say "seeding $state/panel.db and $state/sites_panel.db"
panel migrate
sites_panel migrate
# A source's secret is shown once: kept in .local/secrets/<key id> for the next run.
source_secret() {
	local key_id="$1" brand="$2" file="$state/secrets/$1"
	if panel source list | awk -v k="$key_id" '$1 == k && $NF == "active" { found = 1 } END { exit !found }'; then
		[ -s "$file" ] || die "source $key_id exists but its secret is not in $file — start over with --reset"
	else
		panel source add "$key_id" --kind site --brand "$brand" | awk '$1 == "secret:" { print $2 }' >"$file"
		[ -s "$file" ] || die "panel source add $key_id printed no secret"
	fi
	cat "$file"
}
AQUAFIX_SECRET="$(source_secret aquafix-site aquafix)"
VIFNET_SECRET="$(source_secret vifnet-site vifnet)"
register() {
	sites_panel place register "$1" "$2" >/dev/null 2>>"$state/logs/seed.log" || die "registering $1/$2 failed: see $state/logs/seed.log"
}
for slug in "${AQUAFIX_PLACES[@]}"; do register aquafix "$slug"; done
for slug in "${VIFNET_PLACES[@]}"; do register vifnet "$slug"; done

# ── run ──────────────────────────────────────────────────────────────────────────────────

# Job control: each background job below gets a process group of its own, which a signal
# stops whole — npm and next under `nix run .#dev` included — without touching whoever started
# this script (`kill 0` would, when that is a non-interactive shell sharing our group).
set -m
groups=()
stop_all() {
	trap - INT TERM EXIT
	echo >&2
	say "stopping"
	local pg
	for pg in ${groups[@]+"${groups[@]}"}; do kill -TERM -- "-$pg" 2>/dev/null || true; done
	for _ in $(seq 10); do
		local alive=0
		for pg in ${groups[@]+"${groups[@]}"}; do kill -0 -- "-$pg" 2>/dev/null && alive=1; done
		[ "$alive" = 0 ] && return 0
		sleep 1
	done
	for pg in ${groups[@]+"${groups[@]}"}; do kill -KILL -- "-$pg" 2>/dev/null || true; done
}
trap stop_all EXIT
trap 'stop_all; exit 130' INT TERM

# Each line prefixed with whose it is, and kept in .local/logs/<name>.log.
prefixed() {
	local name="$1"
	shift
	"$@" 2>&1 | tee -a "$state/logs/$name.log" | awk -v p="[$name] " '{ print p $0; fflush() }'
}

# Polls `url` while `pid` lives, at most `tries` seconds.
wait_for() {
	local name="$1" url="$2" tries="$3" pid="$4"
	for _ in $(seq "$tries"); do
		curl -fsS -o /dev/null --max-time 5 "$url" 2>/dev/null && return 0
		kill -0 "$pid" 2>/dev/null || {
			echo "$name stopped before answering $url — see $state/logs/$name.log" >&2
			return 1
		}
		sleep 1
	done
	echo "$name did not answer $url in ${tries}s — see $state/logs/$name.log" >&2
	return 1
}

say "starting sites_panel on $sites_origin"
prefixed sites sites_panel serve --bind "127.0.0.1:$SITES_PORT" &
sites_pid=$!
groups+=("$sites_pid")
wait_for sites "$sites_origin/health" 30 "$sites_pid" || die "sites_panel did not start"

if [ "$review_archive" = 1 ]; then
	say "starting review_archive on $review_archive_origin"
	flock "$REVIEW_ARCHIVE_DATA_DIR/serve.lock" sleep infinity &
	groups+=("$!")
	prefixed review_archive review_archive serve --no-login-check &
	review_archive_pid=$!
	groups+=("$review_archive_pid")
	wait_for review_archive "$review_archive_origin/health" 60 "$review_archive_pid" || die "review_archive did not start"
fi

say "starting the conductor on $origin (dev sign-in: $role)"
prefixed panel panel serve --bind "127.0.0.1:$PANEL_PORT" &
panel_pid=$!
groups+=("$panel_pid")
wait_for panel "$origin/health" 30 "$panel_pid" || die "the conductor did not start"

started=()
# A landing's own dev command (`nix run .#dev`, kitstart's next dev) in its checkout: its leads
# to the conductor's ingest, its places from sites_panel; its leads in a file of ours, analytics off.
start_site() {
	local name="$1" dir="$2" port="$3" brand="$4" key_id="$5" secret="$6"
	if [ ! -f "$dir/package.json" ] || [ ! -f "$dir/flake.nix" ]; then
		echo "skipping $name: $dir is not a kitstart checkout (missing, or older than the Next site — git pull)" >&2
		return 0
	fi
	say "starting $name from $dir on :$port"
	(
		cd "$dir"
		prefixed "$name" env -u POSTHOG_KEY -u POSTHOG_HOST -u LEADS_DB_URL \
			LEAD_WEBHOOK_URL="$origin/api/ingest/v1/events" \
			LEAD_WEBHOOK_KEY_ID="$key_id" \
			LEAD_WEBHOOK_SECRET="$secret" \
			LOCATIONS_API_URL="$sites_origin/api/internal/brands/$brand" \
			LEADS_DB_PATH="$state/$name-leads.db" \
			nix run .#dev
	) &
	groups+=("$!")
	started+=("$name:$port:$!")
}
if [ "$sites" = 1 ]; then
	start_site aquafix "$AQUAFIX_DIR" "$AQUAFIX_PORT" aquafix aquafix-site "$AQUAFIX_SECRET"
	start_site vifnet "$VIFNET_DIR" "$VIFNET_PORT" vifnet vifnet-site "$VIFNET_SECRET"
	for s in ${started[@]+"${started[@]}"}; do
		IFS=: read -r name port pid <<<"$s"
		# The first `nix run .#dev` may install node_modules: minutes, not seconds.
		wait_for "$name" "http://localhost:$port/health" 600 "$pid" || true
	done
fi

{
	echo
	echo "────────────────────────────────────────────────────────────────────────"
	echo " panel    $origin   (opens signed in: dev sign-in, $role)"
	echo "          leads $origin/sites/leads   places $origin/sites/places"
	[ "$review_archive" = 1 ] && echo "          review_archive $origin/review_archive?grafana=true   Grafana $origin/grafana/"
	echo " sites    $sites_origin   (behind the conductor: its /api/internal for the landings)"
	for s in ${started[@]+"${started[@]}"}; do
		case "${s%%:*}" in
		aquafix)
			for slug in "${AQUAFIX_PLACES[@]}"; do echo " aquafix  http://$slug.localhost:$AQUAFIX_PORT/fr"; done
			;;
		vifnet) echo " vifnet   http://localhost:$VIFNET_PORT/fr" ;;
		esac
	done
	echo
	echo " state    $state   (--reset wipes it)   logs $state/logs/"
	echo " stop     Ctrl-C stops everything"
	echo "────────────────────────────────────────────────────────────────────────"
} >&2

# Until Ctrl-C, or until the conductor or sites_panel stops (then everything does). Polled rather
# than `wait -n`, which macOS's bash 3.2 lacks.
while kill -0 "$panel_pid" 2>/dev/null && kill -0 "$sites_pid" 2>/dev/null; do sleep 1; done
say "the conductor or sites_panel stopped — see $state/logs/"
