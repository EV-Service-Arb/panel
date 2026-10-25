#!/usr/bin/env bash
# Re-vendors banking's scope.v1 protos the panel calls (invoices, events, service_auth, and the
# ledger they share) at one commit of EV-Main/banking and records it in REV. See README.md for
# why they are copied rather than a git dependency.
set -euo pipefail
rev="${1:?usage: sync.sh <commit of EV-Main/banking>}"
dir="$(cd "$(dirname "$0")" && pwd)"
for f in invoices events service_auth ledger; do
	gh api -H 'Accept: application/vnd.github.raw' \
		"repos/EV-Main/banking/contents/contracts/proto/scope/v1/$f.proto?ref=$rev" >"$dir/v1/$f.proto"
done
gh api "repos/EV-Main/banking/commits/$rev" --jq .sha >"$dir/REV"
