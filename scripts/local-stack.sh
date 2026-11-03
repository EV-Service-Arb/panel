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
#   nix run .#local-stack -- --watch       every part rebuilt and restarted as its checkout is edited
#   nix run .#local-stack -- --port-offset 1000   every port moved by 1000: a second stack, e.g. a worktree's
#
# The conductor and its front end come from `nix build .#bin` / `.#frontend` of this checkout,
# unless PANEL_BIN / PANEL_WEB_DIR name others (PANEL_BIN=target/debug/panel for a cargo build);
# sites_panel and its bundle from its checkout's, unless SITES_BIN / SITES_MFE_DIR do; review_archive
# likewise (REVIEW_ARCHIVE_BIN / REVIEW_ARCHIVE_MFE_DIR), and Grafana's server (PANEL_GRAFANA_SERVER).
# Under --watch each is cargo's (npm's for the bundles) of its checkout instead, and the *_BIN /
# *_DIR overrides of the builds are refused.
# SITES_DIR, REVIEW_ARCHIVE_DIR, AQUAFIX_DIR and VIFNET_DIR name the checkouts, by default beside
# this one's. Each part's port is what its own dev shell sets (docs/port_range.md).
set -euo pipefail

need="run it as \`nix run .#local-stack\`, or inside \`nix develop\`: the flake sets the conductor's ports"
PANEL_PORT="${PANEL_BIND:?$need}"
PANEL_PORT="${PANEL_PORT##*:}"
WEB_DEV_PORT="${PANEL_WEB_PORT:?$need}"
offset=0
AQUAFIX_PLACES=(royat clermont-ferrand desgenettes lyon-est la-mouche lyon-nord)
VIFNET_PLACES=(vifnet)

reset=0
sites=1
watch=0
role="admin"
while [ $# -gt 0 ]; do
	case "$1" in
	--reset) reset=1 ;;
	--no-sites) sites=0 ;;
	--watch) watch=1 ;;
	--port-offset)
		offset="${2:-}"
		[[ "$offset" =~ ^[0-9]+$ ]] || {
			echo "--port-offset takes a number, not '$offset'" >&2
			exit 2
		}
		shift
		;;
	--role)
		role="${2:-}"
		shift
		;;
	-h | --help)
		cat <<'EOF'
local-stack [--reset] [--no-sites] [--watch] [--role admin|operator] [--port-offset <n>]

  The conductor (dev sign-in, its Grafana at /grafana/), sites_panel, review_archive, aquafix and
  vifnet, wired together, each on the port its dev shell sets. State in .local/ (--reset wipes
  it). Ctrl-C stops everything. See docs/LOCAL.md.
  --watch: every part rebuilt and restarted as its checkout is edited, and the conductor's front end
  from `next dev`, whose origin is then the one to open.
  --port-offset <n>: every port moved by n, beside the stack on the usual ones.
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
# The others sit beside the conductor's main checkout (_sa/{site_conductor,review_archive,
# sites/{panel,aquafix,vifnet}}), also when this is one of its worktrees.
main_checkout="$(dirname "$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)")"
service_arb="$(dirname "$main_checkout")"
SITES_DIR="${SITES_DIR:-$service_arb/sites/panel}"
REVIEW_ARCHIVE_DIR="${REVIEW_ARCHIVE_DIR:-$service_arb/review_archive}"
AQUAFIX_DIR="${AQUAFIX_DIR:-$service_arb/sites/aquafix}"
VIFNET_DIR="${VIFNET_DIR:-$service_arb/sites/vifnet}"

# a stack on other ports is another stack: its own databases, and its own review_archive serve.lock
state="${LOCAL_STACK_DIR:-$root/.local}"
[ "$offset" = 0 ] || state="${LOCAL_STACK_DIR:-$root/.local-$offset}"
if [ "$reset" = 1 ] && [ -d "$state" ]; then
	say "--reset: wiping $state"
	rm -rf "$state"
fi
mkdir -p "$state/logs" "$state/secrets"
# Ignored by its own .gitignore: the repo's is generated, and this is nobody else's business.
printf '*\n' >"$state/.gitignore"

# ── ports ────────────────────────────────────────────────────────────────────────────────

# `shell_port <checkout> <variable>`: the port in what that checkout's dev shell sets the variable
# to (a port, or an address ending in one), moved by --port-offset; nothing for a checkout absent
system="$(nix eval --impure --raw --expr builtins.currentSystem)"
shell_port() {
	[ -f "$1/flake.nix" ] || return 0
	local v
	v="$(nix eval --raw "$1#devShells.$system.default.$2")" || die "$1's dev shell sets no $2"
	echo $((${v##*:} + offset))
}
PANEL_PORT=$((PANEL_PORT + offset))
WEB_DEV_PORT=$((WEB_DEV_PORT + offset))
SITES_PORT="$(shell_port "$SITES_DIR" PANEL_BIND)"
REVIEW_ARCHIVE_PORT="$(shell_port "$REVIEW_ARCHIVE_DIR" REVIEW_ARCHIVE_SERVER_BIND)"
AQUAFIX_PORT="$(shell_port "$AQUAFIX_DIR" PORT)"
VIFNET_PORT="$(shell_port "$VIFNET_DIR" PORT)"

busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
if curl -fsS -o /dev/null --max-time 5 "http://127.0.0.1:$PANEL_PORT/health" 2>/dev/null; then
	echo "a stack already serves http://127.0.0.1:$PANEL_PORT (and http://localhost:$WEB_DEV_PORT under --watch): use it, or --port-offset <n> for another" >&2
	exit 0
fi
ports=("$PANEL_PORT" "$SITES_PORT" "$REVIEW_ARCHIVE_PORT")
[ "$sites" = 1 ] && ports+=("$AQUAFIX_PORT" "$VIFNET_PORT")
[ "$watch" = 1 ] && ports+=("$WEB_DEV_PORT")
for port in "${ports[@]}"; do
	[ -n "$port" ] || continue # a checkout absent, skipped below
	busy "$port" && die "port $port is taken — another local stack, or \`lsof -nP -iTCP:$port -sTCP:LISTEN\` says who"
done

# ── the builds ───────────────────────────────────────────────────────────────────────────

# `in_shell <checkout> <command…>`: in that checkout, under its own dev shell's toolchain
in_shell() {
	local dir="$1"
	shift
	(cd "$dir" && nix develop -c "$@")
}
REVIEW_ARCHIVE_WEB_BUILD="nix run .#build-mfe -- '$state/watch/review_archive-mfe'"
if [ "$watch" = 1 ]; then
	for v in PANEL_BIN PANEL_WEB_DIR SITES_BIN SITES_MFE_DIR REVIEW_ARCHIVE_BIN REVIEW_ARCHIVE_MFE_DIR; do
		[ -z "${!v:-}" ] || die "--watch builds every part from its checkout: unset $v"
	done
	[ -f "$SITES_DIR/flake.nix" ] || die "no sites_panel checkout at $SITES_DIR: SITES_DIR names it"
	[ -f "$REVIEW_ARCHIVE_DIR/flake.nix" ] || die "no review_archive checkout at $REVIEW_ARCHIVE_DIR: REVIEW_ARCHIVE_DIR names it"
	say "building the conductor, sites_panel, review_archive and the bundles from their checkouts"
	in_shell "$root" cargo b -q -p panel_server
	in_shell "$SITES_DIR" cargo b -q -p panel_server
	in_shell "$REVIEW_ARCHIVE_DIR" cargo b -q -p review_archive_server
	(cd "$REVIEW_ARCHIVE_DIR" && nix run .#build-mfe -- "$state/watch/review_archive-mfe")
	(cd "$SITES_DIR/frontend" && { [ -d node_modules ] || npm ci; } && npm run -s build)
	PANEL_BIN="$root/target/debug/panel"
	SITES_BIN="$SITES_DIR/target/debug/sites_panel"
	SITES_MFE_DIR="$SITES_DIR/frontend/dist"
	REVIEW_ARCHIVE_BIN="$REVIEW_ARCHIVE_DIR/target/debug/review_archive"
	REVIEW_ARCHIVE_MFE_DIR="$state/watch/review_archive-mfe"
fi
if [ -z "${PANEL_BIN:-}" ]; then
	say "building the conductor (nix build .#bin)"
	nix build "$root#bin" --out-link "$state/build/bin"
	PANEL_BIN="$state/build/bin/bin/panel"
fi
if [ "$watch" = 1 ]; then
	PANEL_WEB_DIR=""
elif [ -z "${PANEL_WEB_DIR:-}" ]; then
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
# The browser's origin: the sign-in sends it back there, and the live socket must come from it.
public_origin="$origin"
[ "$watch" = 1 ] && public_origin="http://localhost:$WEB_DEV_PORT"
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
		PANEL_PUBLIC_ORIGIN="$public_origin" \
		PANEL_DEV_SIGN_IN="sa:$role" \
		PANEL_WEB_DIR="$PANEL_WEB_DIR" \
		PANEL_ASSERTION_KEY="$assertion_key" \
		PANEL_SITES_URL="$sites_origin" \
		PANEL_REVIEW_ARCHIVE_URL="$review_archive_origin" \
		PANEL_GRAFANA_SERVER="$PANEL_GRAFANA_SERVER" \
		PANEL_GRAFANA_DIR="$state/grafana" \
		PANEL_PLAYBOOK_URL="http://127.0.0.1:9" \
		TOKENS_START_CAP_MULTIPLE=1 \
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
EOF
	env -u SENTRY_DSN -u TELEGRAM_BOT_TOKEN -u GOOGLE_MAPS_KEY \
		APP_ENV=development \
		PANEL_ASSERTION_KEYS="$assertion_public" \
		"$REVIEW_ARCHIVE_BIN" --config "$state/review_archive/config.toml" "$@"
}

# ── seed: idempotent, so every run may do it ─────────────────────────────────────────────

say "seeding $state/panel.db and $state/sites_panel.db"
fresh=0
[ -e "$state/panel.db" ] || fresh=1
panel migrate
sites_panel migrate
# Tokens and GMBs open with the import from review_archive: here, of nothing, into a new panel.db
# (one copied in has its own).
if [ "$fresh" = 1 ]; then
	echo '{"people": [], "gmails": [], "gmbs": [], "ledger": [], "kickbacks": []}' >"$state/review-archive-export.json"
	panel import review-archive "$state/review-archive-export.json"
fi
# A source's secret is shown once: kept in .local/secrets/<key id> for the next run. Its origin
# is the landing's, set again for a key an older run made without one (the same origin is no change).
source_secret() {
	local key_id="$1" brand="$2" origin="$3" file="$state/secrets/$1"
	if panel source list | awk -v k="$key_id" '$1 == k && $NF == "active" { found = 1 } END { exit !found }'; then
		[ -s "$file" ] || die "source $key_id exists but its secret is not in $file — start over with --reset"
		panel source set-origin "$key_id" "$origin"
	else
		panel source add "$key_id" --kind site --brand "$brand" --origin "$origin" | awk '$1 == "secret:" { print $2 }' >"$file"
		[ -s "$file" ] || die "panel source add $key_id printed no secret"
	fi
	cat "$file"
}
# a landing with no checkout has no port, and no source either
AQUAFIX_SECRET=""
VIFNET_SECRET=""
[ -z "$AQUAFIX_PORT" ] || AQUAFIX_SECRET="$(source_secret aquafix-site aquafix "http://localhost:$AQUAFIX_PORT")"
[ -z "$VIFNET_PORT" ] || VIFNET_SECRET="$(source_secret vifnet-site vifnet "http://localhost:$VIFNET_PORT")"
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

# `watched <checkout> <watchexec's filters…> -- <command>`: <command> (bash, the functions above
# included) in that checkout's dev shell, stopped and run again as the files watched change
export -f panel sites_panel review_archive
export state public_origin role PANEL_WEB_DIR assertion_key assertion_public sites_origin review_archive_origin \
	PANEL_GRAFANA_SERVER PANEL_BIN SITES_BIN SITES_MFE_DIR REVIEW_ARCHIVE_BIN REVIEW_ARCHIVE_MFE_DIR REVIEW_ARCHIVE_DATA_DIR REVIEW_ARCHIVE_PORT
watched() {
	local dir="$1"
	shift
	in_shell "$dir" watchexec --shell=bash --restart --stop-signal SIGTERM --stop-timeout 10s --debounce 500ms "$@"
}
# A part under --watch: built again and restarted on its checkout's changes; else the build above.
serving() {
	local name="$1" dir="$2" build="$3"
	shift 3
	if [ "$watch" = 1 ]; then
		prefixed "$name" watched "$dir" -w crates -w Cargo.toml -w Cargo.lock "${@:2}" -- "$build && $1"
	else
		prefixed "$name" bash -c "$1"
	fi
}

say "starting sites_panel on $sites_origin"
serving sites "$SITES_DIR" "cargo b -q -p panel_server" "sites_panel serve --bind 127.0.0.1:$SITES_PORT" -e rs,toml,sql,lock &
sites_pid=$!
groups+=("$sites_pid")
wait_for sites "$sites_origin/health" 30 "$sites_pid" || die "sites_panel did not start"

if [ "$review_archive" = 1 ]; then
	say "starting review_archive on $review_archive_origin"
	mkdir -p "$REVIEW_ARCHIVE_DATA_DIR"
	flock "$REVIEW_ARCHIVE_DATA_DIR/serve.lock" sleep infinity &
	groups+=("$!")
	serving review_archive "$REVIEW_ARCHIVE_DIR" "cargo b -q -p review_archive_server" "review_archive serve --no-login-check" \
		-w grafana -i 'crates/review_archive_web/**' -e rs,toml,sql,json,lock &
	review_archive_pid=$!
	groups+=("$review_archive_pid")
	wait_for review_archive "$review_archive_origin/health" 60 "$review_archive_pid" || die "review_archive did not start"
fi

say "starting the conductor on $origin (dev sign-in: $role)"
serving panel "$root" "cargo b -q -p panel_server" "panel serve --bind 127.0.0.1:$PANEL_PORT" -i 'crates/panel_gen/**' -e rs,toml,sql,lock &
panel_pid=$!
groups+=("$panel_pid")
wait_for panel "$origin/health" 30 "$panel_pid" || die "the conductor did not start"

if [ "$watch" = 1 ]; then
	say "watching the bundles of review_archive and sites"
	prefixed review_archive-mfe watched "$REVIEW_ARCHIVE_DIR" --postpone -w crates/review_archive_web -w crates/review_archive_client -w crates/review_archive_core -e rs,css,html,toml -- "$REVIEW_ARCHIVE_WEB_BUILD" &
	groups+=("$!")
	prefixed sites-mfe watched "$SITES_DIR" --postpone -w frontend/src -w frontend/messages -- "cd frontend && npm run -s build" &
	groups+=("$!")
	say "starting the front end's next dev on $public_origin"
	(
		cd "$root/frontend"
		[ -d node_modules ] || prefixed web npm ci
		# the image's build switches (flake.nix, the frontend's env)
		NEXT_PUBLIC_SIGNIN_OVERLAY=1 NEXT_PUBLIC_WALLET_PAGE=1 PANEL_DEV_BACKEND="$origin" \
			prefixed web npx next dev --port "$WEB_DEV_PORT"
	) &
	web_pid=$!
	groups+=("$web_pid")
	wait_for web "$public_origin/" 300 "$web_pid" || die "next dev did not start"
fi

started=()
# A landing's own dev command (`nix run .#dev`, kitstart's next dev) in its checkout: its leads
# to the conductor's ingest, its places from sites_panel; its leads in a file of ours, analytics off.
start_site() {
	local name="$1" dir="$2" port="$3" key_id="$4" secret="$5"
	if [ ! -f "$dir/package.json" ] || [ ! -f "$dir/flake.nix" ]; then
		echo "skipping $name: $dir is not a kitstart checkout (missing, or older than the Next site — git pull)" >&2
		return 0
	fi
	say "starting $name from $dir on :$port"
	(
		cd "$dir"
		prefixed "$name" env -u POSTHOG_KEY -u POSTHOG_HOST -u LEADS_DB_URL \
			PORT="$port" \
			LEAD_WEBHOOK_URL="$origin/api/ingest/v1/events" \
			LEAD_WEBHOOK_KEY_ID="$key_id" \
			LEAD_WEBHOOK_SECRET="$secret" \
			SITES_PANEL_INTERNAL_URL="$sites_origin/api/internal" \
			LEADS_DB_PATH="$state/$name-leads.db" \
			nix run .#dev
	) &
	groups+=("$!")
	started+=("$name:$port:$!")
}
if [ "$sites" = 1 ]; then
	start_site aquafix "$AQUAFIX_DIR" "$AQUAFIX_PORT" aquafix-site "$AQUAFIX_SECRET"
	start_site vifnet "$VIFNET_DIR" "$VIFNET_PORT" vifnet-site "$VIFNET_SECRET"
	for s in ${started[@]+"${started[@]}"}; do
		IFS=: read -r name port pid <<<"$s"
		# The first `nix run .#dev` may install node_modules: minutes, not seconds.
		wait_for "$name" "http://localhost:$port/health" 600 "$pid" || true
	done
fi

{
	echo
	echo "────────────────────────────────────────────────────────────────────────"
	echo " panel    $public_origin   (opens signed in: dev sign-in, $role)"
	echo "          leads $public_origin/sites/leads   places $public_origin/sites/places   connected sites $public_origin/sources"
	[ "$review_archive" = 1 ] && echo "          review_archive $public_origin/review_archive?grafana=true   Grafana $public_origin/grafana/"
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
	[ "$watch" = 1 ] && echo " watch    every part is rebuilt and restarted as its checkout is edited"
	echo " stop     Ctrl-C stops everything"
	echo "────────────────────────────────────────────────────────────────────────"
} >&2

# Until Ctrl-C, or until the conductor or sites_panel stops (then everything does). Polled rather
# than `wait -n`, which macOS's bash 3.2 lacks.
while kill -0 "$panel_pid" 2>/dev/null && kill -0 "$sites_pid" 2>/dev/null; do sleep 1; done
say "the conductor or sites_panel stopped — see $state/logs/"
