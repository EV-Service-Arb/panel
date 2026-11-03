# Local ports

Everything of Service-Arb run on a developer's machine listens in `SA_PORT_RANGE`, at the top of
this repository's flake.nix, each repository on ten ports of its own. A part is always on the same
port, so a second debugging session finds the stack already up and reuses it instead of starting
another; a part not answering on its port is not running here, and nothing relies on a copy
elsewhere (a worktree's stack, under `nix run .#local-stack -- --port-offset <n>`).

The tens, in order from the start of the range:

1. site_conductor
2. review_archive
3. sites: sites/panel first, then the landings (aquafix, vifnet)
4. playbook
5. verif_tools

A new repository takes the next ten. Each sets the first of its own at the top of its flake.nix
(`SA_PORTS`; a landing, its one port, `SA_PORT`), linking here, and derives the rest. Its dev shell
hands them to the code as the variables it reads (`PANEL_BIND`, `REVIEW_ARCHIVE_SERVER_BIND`,
`PORT`, …), which fails to start without them; the conductor's `local-stack` reads each part's port
out of that part's own dev shell.

In the cluster every container keeps its own port (each flake's `port`, the contract the manifests
are generated from).
