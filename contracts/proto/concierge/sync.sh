#!/usr/bin/env bash
# Re-vendors concierge's auth.proto and directory.proto at one commit of EV-Main/concierge
# and records it in REV. See README.md for why they are copied rather than a git dependency.
set -euo pipefail
rev="${1:?usage: sync.sh <commit of EV-Main/concierge>}"
dir="$(cd "$(dirname "$0")" && pwd)"
for f in auth directory; do
	gh api -H 'Accept: application/vnd.github.raw' \
		"repos/EV-Main/concierge/contents/contracts/proto/concierge/v1/$f.proto?ref=$rev" >"$dir/v1/$f.proto"
done
gh api "repos/EV-Main/concierge/commits/$rev" --jq .sha >"$dir/REV"
