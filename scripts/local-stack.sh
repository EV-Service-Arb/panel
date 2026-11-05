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
#   nix run .#local-stack -- --reset --from-prod   prod's databases, from their litestream replicas
#   nix run .#dev [-- --port <n>]          the conductor, sites_panel and review_archive under --watch,
#                                          each started unless it already serves; the conductor on n
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
# whom dev sign-in is, whatever --role: with --from-prod, prod's own account
DEV_EMAIL="admin@evinvest.ltd"
# prod's host, as ~/.ssh/config names it: --from-prod reads review_archive's blobs off its volume
PROD_SSH="rpi5-ts"
AQUAFIX_PLACES=(royat clermont-ferrand desgenettes lyon-est la-mouche lyon-nord)
VIFNET_PLACES=(vifnet)

reset=0
sites=1
watch=0
dev=0
own_port=""
from_prod=0
role="admin"
while [ $# -gt 0 ]; do
	case "$1" in
	--reset) reset=1 ;;
	--no-sites) sites=0 ;;
	--watch) watch=1 ;;
	--from-prod) from_prod=1 ;;
	--dev) dev=1 ;;
	--port)
		own_port="${2:-}"
		[[ "$own_port" =~ ^[0-9]+$ ]] || {
			echo "--port takes a number, not '$own_port'" >&2
			exit 2
		}
		shift
		;;
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
local-stack [--reset] [--no-sites] [--watch] [--role admin|operator] [--port-offset <n>] [--from-prod]
dev [--port <n>] [--reset] [--role admin|operator] [--from-prod]

  The conductor (dev sign-in, its Grafana at /grafana/), sites_panel, review_archive, aquafix and
  vifnet, wired together, each on the port its dev shell sets. State in .local/ (--reset wipes
  it). Ctrl-C stops everything. See docs/LOCAL.md.
  --watch: every part rebuilt and restarted as its checkout is edited, and the conductor's front end
  from `next dev`, whose origin is then the one to open.
  --port-offset <n>: every port moved by n, beside the stack on the usual ones.
  --from-prod: prod's panel.db, sites_panel.db and review_archive.db from their litestream replicas
  (the keys from ../devops's sops), and review_archive's blobs over ssh (PROD_SSH), into a state
  with none of them: with --reset.
  Dev sign-in is one user whatever --role (DEV_EMAIL, at the top of the script): prod's, when copied.
  dev (--dev): --watch without the landings, each of the conductor, sites_panel and review_archive
  started unless it already answers on its port, a peer without a checkout skipped with a warning.
  --port <n>: the conductor on n and its next dev on n+1, in SA_PORT_RANGE, with a state of its own.
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
if [ "$dev" = 1 ]; then
	watch=1
	sites=0
fi
[ -z "$own_port" ] || [ "$dev" = 1 ] || {
	echo "--port is dev's: local-stack moves every port with --port-offset" >&2
	exit 2
}

say() { printf '\033[1m▶ %s\033[0m\n' "$*" >&2; }
die() {
	printf '\033[31m✗ %s\033[0m\n' "$*" >&2
	exit 1
}
warn() { printf '\033[33m! %s\033[0m\n' "$*" >&2; }

root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "run this from inside the conductor's checkout"
[ -f "$root/crates/server/Cargo.toml" ] || die "$root is not the conductor's checkout"
# The others sit beside the conductor's main checkout (_sa/{site_conductor,review_archive,
# sites/{panel,aquafix,vifnet}}), also when this is one of its worktrees.
main_checkout="$(dirname "$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)")"
service_arb="$(dirname "$main_checkout")"
if [ "$dev" = 1 ]; then
	# DEV_PATH_<peer>: its user's path, flake.nix devUsers
	dev_dir() {
		local p="${1:-../$2}"
		[[ "$p" = /* ]] || p="$main_checkout/$p"
		realpath -m "$p"
	}
	SITES_DIR="${SITES_DIR:-$(dev_dir "${DEV_PATH_sites_panel:-}" sites_panel)}"
	REVIEW_ARCHIVE_DIR="${REVIEW_ARCHIVE_DIR:-$(dev_dir "${DEV_PATH_review_archive:-}" review_archive)}"
fi
SITES_DIR="${SITES_DIR:-$service_arb/sites/panel}"
REVIEW_ARCHIVE_DIR="${REVIEW_ARCHIVE_DIR:-$service_arb/review_archive}"
AQUAFIX_DIR="${AQUAFIX_DIR:-$service_arb/sites/aquafix}"
VIFNET_DIR="${VIFNET_DIR:-$service_arb/sites/vifnet}"

# a stack on other ports is another stack: its own databases, and its own review_archive serve.lock
state="${LOCAL_STACK_DIR:-$root/.local}"
[ "$offset" = 0 ] || state="${LOCAL_STACK_DIR:-$root/.local-$offset}"
# a conductor on a port of its own beside the usual one: never two on one panel.db
[ -z "$own_port" ] || state="${LOCAL_STACK_DIR:-$root/.local-port-$own_port}"
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
if [ -n "$own_port" ]; then
	range="${SA_PORT_RANGE:?$need}"
	[ "$own_port" -ge "${range%-*}" ] && [ "$((own_port + 1))" -le "${range#*-}" ] ||
		die "--port $own_port: the conductor and its next dev take $own_port and $((own_port + 1)), both in SA_PORT_RANGE $range (docs/port_range.md)"
	PANEL_PORT="$own_port"
	WEB_DEV_PORT=$((own_port + 1))
else
	PANEL_PORT=$((PANEL_PORT + offset))
	WEB_DEV_PORT=$((WEB_DEV_PORT + offset))
fi
SITES_PORT="$(shell_port "$SITES_DIR" PANEL_BIND)"
REVIEW_ARCHIVE_PORT="$(shell_port "$REVIEW_ARCHIVE_DIR" REVIEW_ARCHIVE_SERVER_BIND)"
AQUAFIX_PORT=""
VIFNET_PORT=""
if [ "$sites" = 1 ] || [ "$dev" = 0 ]; then
	AQUAFIX_PORT="$(shell_port "$AQUAFIX_DIR" PORT)"
	VIFNET_PORT="$(shell_port "$VIFNET_DIR" PORT)"
fi

busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
answers() { curl -fsS -o /dev/null --max-time 5 "http://127.0.0.1:$1/health" 2>/dev/null; }
# What this run starts: under dev, each part not already answering on its port; else all of them.
run_panel=1
run_sites=1
run_review_archive=1
if [ "$dev" = 1 ]; then
	if answers "$PANEL_PORT"; then
		say "the conductor already serves http://127.0.0.1:$PANEL_PORT: using it"
		run_panel=0
	fi
	if [ -z "$SITES_PORT" ]; then
		warn "skipping sites_panel: no checkout at $SITES_DIR (flake.nix devUsers, or SITES_DIR, names another)"
		run_sites=0
	elif answers "$SITES_PORT"; then
		say "sites_panel already serves http://127.0.0.1:$SITES_PORT: using it"
		run_sites=0
	fi
	if [ -z "$REVIEW_ARCHIVE_PORT" ]; then
		warn "skipping review_archive: no checkout at $REVIEW_ARCHIVE_DIR (flake.nix devUsers, or REVIEW_ARCHIVE_DIR, names another)"
		run_review_archive=0
	elif answers "$REVIEW_ARCHIVE_PORT"; then
		say "review_archive already serves http://127.0.0.1:$REVIEW_ARCHIVE_PORT: using it"
		run_review_archive=0
	fi
	if [ "$run_panel$run_sites$run_review_archive" = 000 ]; then
		if busy "$WEB_DEV_PORT"; then say "open http://localhost:$WEB_DEV_PORT"; else say "open http://127.0.0.1:$PANEL_PORT"; fi
		exit 0
	fi
elif answers "$PANEL_PORT"; then
	echo "a stack already serves http://127.0.0.1:$PANEL_PORT (and http://localhost:$WEB_DEV_PORT under --watch): use it, or --port-offset <n> for another" >&2
	exit 0
fi
ports=()
[ "$run_panel" = 1 ] && ports+=("$PANEL_PORT")
[ "$run_sites" = 1 ] && ports+=("$SITES_PORT")
[ "$run_review_archive" = 1 ] && ports+=("$REVIEW_ARCHIVE_PORT")
[ "$sites" = 1 ] && ports+=("$AQUAFIX_PORT" "$VIFNET_PORT")
[ "$watch" = 1 ] && [ "$run_panel" = 1 ] && ports+=("$WEB_DEV_PORT")
for port in "${ports[@]}"; do
	[ -n "$port" ] || continue # a checkout absent, skipped below
	busy "$port" && die "port $port is taken — another local stack, or \`lsof -nP -iTCP:$port -sTCP:LISTEN\` says who"
done

# ── prod's data ──────────────────────────────────────────────────────────────────────────

REVIEW_ARCHIVE_DATA_DIR="${REVIEW_ARCHIVE_DATA_DIR:-$state/review_archive}"
if [ "$from_prod" = 1 ]; then
	DEVOPS_DIR="${DEVOPS_DIR:-$(dirname "$service_arb")/devops}"
	GITOPS_DIR="${GITOPS_DIR:-$(dirname "$service_arb")/gitops}"
	copies=("$state/panel.db:panel-data/panel.db" "$state/sites_panel.db:sites-panel-data/sites_panel.db"
		"$REVIEW_ARCHIVE_DATA_DIR/review_archive.db:review-archive-data/review_archive.db")
	for c in "${copies[@]}"; do
		[ ! -e "${c%%:*}" ] || die "--from-prod restores into a state without ${c%%:*}: add --reset"
	done
	say "restoring prod's databases from their litestream replicas"
	# the replicas as gitops main configures them, not as an old checkout of it says
	git -C "$GITOPS_DIR" fetch -q origin main
	git -C "$GITOPS_DIR" show FETCH_HEAD:clusters/rpi5/tenants/service-arb/manifests.yaml |
		yq eval-all '[select(.kind == "ConfigMap" and .data["litestream.yml"])][0].data["litestream.yml"]' >"$state/litestream.yml"
	secret() { sops -d --extract "[\"$1\"]" "$DEVOPS_DIR/nix/secrets/host.json"; }
	r2_key="$(secret r2_access_key_id)"
	r2_secret="$(secret r2_secret_access_key)"
	mkdir -p "$REVIEW_ARCHIVE_DATA_DIR"
	for c in "${copies[@]}"; do
		# the caller's AWS_* would win over litestream's own variables
		env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY LITESTREAM_ACCESS_KEY_ID="$r2_key" LITESTREAM_SECRET_ACCESS_KEY="$r2_secret" \
			litestream restore -config "$state/litestream.yml" -o "${c%%:*}" "/pvc/${c#*:}"
	done
	unset r2_key r2_secret
	say "copying review_archive's blobs from $PROD_SSH"
	# not replicated: read off the volume on the host
	ssh -o BatchMode=yes "$PROD_SSH" bash -s <<'REMOTE' | tar -C "$REVIEW_ARCHIVE_DATA_DIR" -xf -
set -euo pipefail
pv="$(sudo -n k3s kubectl -n service-arb get pvc review-archive-data -o jsonpath='{.spec.volumeName}')"
sudo -n tar -C "$(sudo -n k3s kubectl get pv "$pv" -o jsonpath='{.spec.local.path}')" -cf - blobs
REMOTE
fi

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
	# also when another conductor serves: `panel gen-data-key` makes the state's key
	say "building the conductor from $root"
	in_shell "$root" cargo b -q -p sa_server
	PANEL_BIN="$root/target/debug/panel"
	if [ "$run_sites" = 1 ] && [ -z "${DEV_COMMAND_sites_panel:-}" ]; then
		[ -f "$SITES_DIR/flake.nix" ] || die "no sites_panel checkout at $SITES_DIR: SITES_DIR names it"
		say "building sites_panel and its bundle from $SITES_DIR"
		in_shell "$SITES_DIR" cargo b -q -p panel_server
		(cd "$SITES_DIR/frontend" && { [ -d node_modules ] || npm ci; } && npm run -s build)
		SITES_BIN="$SITES_DIR/target/debug/sites_panel"
		SITES_MFE_DIR="$SITES_DIR/frontend/dist"
	fi
	if [ "$run_review_archive" = 1 ] && [ -z "${DEV_COMMAND_review_archive:-}" ]; then
		[ -f "$REVIEW_ARCHIVE_DIR/flake.nix" ] || die "no review_archive checkout at $REVIEW_ARCHIVE_DIR: REVIEW_ARCHIVE_DIR names it"
		say "building review_archive and its bundle from $REVIEW_ARCHIVE_DIR"
		in_shell "$REVIEW_ARCHIVE_DIR" cargo b -q -p review_archive_server
		(cd "$REVIEW_ARCHIVE_DIR" && nix run .#build-mfe -- "$state/watch/review_archive-mfe")
		REVIEW_ARCHIVE_BIN="$REVIEW_ARCHIVE_DIR/target/debug/review_archive"
		REVIEW_ARCHIVE_MFE_DIR="$state/watch/review_archive-mfe"
	fi
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
if [ "$run_sites" = 1 ] && [ "$watch" = 0 ] && [ -z "${SITES_BIN:-}" ]; then
	[ -f "$SITES_DIR/flake.nix" ] || die "no sites_panel checkout at $SITES_DIR: SITES_DIR names it"
	say "building sites_panel ($SITES_DIR#bin)"
	nix build "$SITES_DIR#bin" --out-link "$state/build/sites"
	SITES_BIN="$state/build/sites/bin/sites_panel"
fi
if [ "$run_sites" = 1 ] && [ "$watch" = 0 ] && [ -z "${SITES_MFE_DIR:-}" ]; then
	say "building the sites' bundle ($SITES_DIR#frontend)"
	nix build "$SITES_DIR#frontend" --out-link "$state/build/sites-mfe"
	SITES_MFE_DIR="$state/build/sites-mfe"
fi
[ "$run_sites" = 0 ] || [ -n "${DEV_COMMAND_sites_panel:-}" ] || [ -x "$SITES_BIN" ] || die "SITES_BIN $SITES_BIN is not an executable"
if [ "$run_panel" = 1 ] && [ -z "${PANEL_GRAFANA_SERVER:-}" ]; then
	say "building Grafana (nix build .#grafana-server)"
	nix build "$root#grafana-server" --out-link "$state/build/grafana-server"
	PANEL_GRAFANA_SERVER="$state/build/grafana-server"
fi
if [ "$run_review_archive" = 1 ] && [ "$watch" = 0 ] && { [ -z "${REVIEW_ARCHIVE_BIN:-}" ] || [ -z "${REVIEW_ARCHIVE_MFE_DIR:-}" ]; }; then
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
		run_review_archive=0
	fi
fi

# ── the environments ─────────────────────────────────────────────────────────────────────

[ -s "$state/data-key" ] || "$PANEL_BIN" gen-data-key >"$state/data-key"
origin="http://127.0.0.1:$PANEL_PORT"
# The browser's origin: the sign-in sends it back there, and the live socket must come from it.
public_origin="$origin"
if [ "$watch" = 1 ] && { [ "$run_panel" = 1 ] || busy "$WEB_DEV_PORT"; }; then
	public_origin="http://localhost:$WEB_DEV_PORT"
fi
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
		PANEL_DEV_SIGN_IN_EMAIL="$DEV_EMAIL" \
		PANEL_WEB_DIR="$PANEL_WEB_DIR" \
		PANEL_ASSERTION_KEY="$assertion_key" \
		PANEL_SITES_URL="$sites_origin" \
		PANEL_REVIEW_ARCHIVE_URL="$review_archive_origin" \
		PANEL_GRAFANA_SERVER="$PANEL_GRAFANA_SERVER" \
		PANEL_GRAFANA_DIR="$state/grafana" \
		PANEL_PLAYBOOK_URL="http://127.0.0.1:9" \
		TOKENS_START_CAP_MULTIPLE=1 \
		GOOGLE_MAPS_KEYS="${GOOGLE_MAPS_KEYS:-${GOOGLE_MAPS_KEY:+$GOOGLE_MAPS_KEY${GOOGLE_MAPS_KEY_ICOS1:+,$GOOGLE_MAPS_KEY_ICOS1}}}" \
		GOOGLE_MAPS_BROWSER_KEY="${GOOGLE_MAPS_BROWSER_KEY:-${GOOGLE_MAPS_KEY:-}}" \
		GOOGLE_MAPS_MAP_ID="${GOOGLE_MAPS_MAP_ID:-${GOOGLE_MAPS_KEY:+DEMO_MAP_ID}}" \
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

# a conductor or sites_panel already serving has its own state, seeded by the run that started it
if [ "$run_panel" = 1 ]; then
	say "seeding $state/panel.db"
	fresh=0
	[ -e "$state/panel.db" ] || fresh=1
	panel migrate
	# Tokens and GMBs open with the import from review_archive: here, of nothing, into a new panel.db
	# (one copied in has its own).
	if [ "$fresh" = 1 ]; then
		echo '{"people": [], "gmails": [], "gmbs": [], "ledger": [], "kickbacks": []}' >"$state/review-archive-export.json"
		panel import review-archive "$state/review-archive-export.json"
	fi
	# The stack's own keys, `<brand>-local`: a copy of prod has prod's `<brand>-site`, whose secrets
	# are not here. A secret is shown once: kept in .local/secrets/<key id> for the next run. Its origin
	# is the landing's; the same origin again is no change, another one is refused.
	source_key() {
		local key_id="$1" brand="$2" origin="$3" file="$state/secrets/$1"
		if panel source list | awk -v k="$key_id" '$1 == k && $NF == "active" { found = 1 } END { exit !found }'; then
			[ -s "$file" ] || die "source $key_id exists but its secret is not in $file — start over with --reset"
			panel source set-origin "$key_id" "$origin" || die "source $key_id has another origin than $origin — start over with --reset"
		else
			panel source add "$key_id" --kind site --brand "$brand" --origin "$origin" | awk '$1 == "secret:" { print $2 }' >"$file"
			[ -s "$file" ] || die "panel source add $key_id printed no secret"
		fi
	}
	# a landing with no checkout has no port, and no source either
	[ -z "$AQUAFIX_PORT" ] || source_key aquafix-local aquafix "http://localhost:$AQUAFIX_PORT"
	[ -z "$VIFNET_PORT" ] || source_key vifnet-local vifnet "http://localhost:$VIFNET_PORT"
fi
if [ "$run_sites" = 1 ] && [ -z "${DEV_COMMAND_sites_panel:-}" ]; then
	say "seeding $state/sites_panel.db"
	sites_panel migrate
	register() {
		sites_panel place register "$1" "$2" >/dev/null 2>>"$state/logs/seed.log" || die "registering $1/$2 failed: see $state/logs/seed.log"
	}
	for slug in "${AQUAFIX_PLACES[@]}"; do register aquafix "$slug"; done
	for slug in "${VIFNET_PLACES[@]}"; do register vifnet "$slug"; done
fi

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
export state public_origin role DEV_EMAIL PANEL_WEB_DIR assertion_key assertion_public sites_origin review_archive_origin \
	PANEL_GRAFANA_SERVER PANEL_BIN SITES_BIN SITES_MFE_DIR REVIEW_ARCHIVE_BIN REVIEW_ARCHIVE_MFE_DIR REVIEW_ARCHIVE_DATA_DIR REVIEW_ARCHIVE_PORT \
	PANEL_PORT SITES_PORT
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

# The parts whose stop stops the rest.
essential=()
# `custom <name> <checkout> <command>`: a peer served by its user's DEV_COMMAND_<peer> (flake.nix
# devUsers) in place of the stack's own, in its checkout, on the port its dev shell sets
custom() {
	say "starting $1 from $2 with \`$3\`"
	(cd "$2" && prefixed "$1" bash -c "$3") &
	groups+=("$!")
	essential+=("$!")
}

if [ "$run_sites" = 1 ]; then
	if [ -n "${DEV_COMMAND_sites_panel:-}" ]; then
		custom sites "$SITES_DIR" "$DEV_COMMAND_sites_panel"
		wait_for sites "$sites_origin/health" 600 "$!" || die "sites_panel did not start"
	else
		say "starting sites_panel on $sites_origin"
		serving sites "$SITES_DIR" "cargo b -q -p panel_server" "sites_panel serve --bind 127.0.0.1:$SITES_PORT" -e rs,toml,sql,lock &
		groups+=("$!")
		essential+=("$!")
		wait_for sites "$sites_origin/health" 30 "$!" || die "sites_panel did not start"
		if [ "$watch" = 1 ]; then
			prefixed sites-mfe watched "$SITES_DIR" --postpone -w frontend/src -w frontend/messages -- "cd frontend && npm run -s build" &
			groups+=("$!")
		fi
	fi
fi

if [ "$run_review_archive" = 1 ]; then
	if [ -n "${DEV_COMMAND_review_archive:-}" ]; then
		custom review_archive "$REVIEW_ARCHIVE_DIR" "$DEV_COMMAND_review_archive"
		wait_for review_archive "$review_archive_origin/health" 600 "$!" || die "review_archive did not start"
	else
		say "starting review_archive on $review_archive_origin"
		mkdir -p "$REVIEW_ARCHIVE_DATA_DIR"
		flock "$REVIEW_ARCHIVE_DATA_DIR/serve.lock" sleep infinity &
		groups+=("$!")
		serving review_archive "$REVIEW_ARCHIVE_DIR" "cargo b -q -p review_archive_server" "review_archive serve --no-login-check" \
			-w grafana -i 'crates/review_archive_web/**' -e rs,toml,sql,json,lock &
		groups+=("$!")
		essential+=("$!")
		wait_for review_archive "$review_archive_origin/health" 60 "$!" || die "review_archive did not start"
		if [ "$watch" = 1 ]; then
			prefixed review_archive-mfe watched "$REVIEW_ARCHIVE_DIR" --postpone -w crates/review_archive_web -w crates/review_archive_client -w crates/review_archive_core -e rs,css,html,toml -- "$REVIEW_ARCHIVE_WEB_BUILD" &
			groups+=("$!")
		fi
	fi
fi

if [ "$run_panel" = 1 ]; then
	say "starting the conductor on $origin (dev sign-in: $role)"
	serving panel "$root" "cargo b -q -p sa_server" "panel serve --bind 127.0.0.1:$PANEL_PORT" -i 'crates/gen/**' -e rs,toml,sql,lock &
	groups+=("$!")
	essential+=("$!")
	wait_for panel "$origin/health" 30 "$!" || die "the conductor did not start"
fi

if [ "$watch" = 1 ] && [ "$run_panel" = 1 ]; then
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
	local name="$1" dir="$2" port="$3" key_id="$4"
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
			LEAD_WEBHOOK_SECRET="$(cat "$state/secrets/$key_id")" \
			SITES_PANEL_INTERNAL_URL="$sites_origin/api/internal" \
			LEADS_DB_PATH="$state/$name-leads.db" \
			nix run .#dev
	) &
	groups+=("$!")
	started+=("$name:$port:$!")
}
if [ "$sites" = 1 ]; then
	start_site aquafix "$AQUAFIX_DIR" "$AQUAFIX_PORT" aquafix-local
	start_site vifnet "$VIFNET_DIR" "$VIFNET_PORT" vifnet-local
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
	answers "$REVIEW_ARCHIVE_PORT" && echo "          review_archive $public_origin/review_archive?grafana=true   Grafana $public_origin/grafana/"
	answers "$SITES_PORT" && echo " sites    $sites_origin   (behind the conductor: its /api/internal for the landings)"
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
	echo " stop     Ctrl-C stops what this run started"
	echo "────────────────────────────────────────────────────────────────────────"
} >&2

# Until Ctrl-C, or until one of the essential stops (then everything does). Polled rather than
# `wait -n`, which macOS's bash 3.2 lacks.
running() {
	local pid
	for pid in "${essential[@]}"; do kill -0 "$pid" 2>/dev/null || return 1; done
}
while running; do sleep 1; done
say "a part stopped — see $state/logs/"
