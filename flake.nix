{
  nixConfig = {
    extra-substituters = [ "https://valeratrades.cachix.org" ];
    extra-trusted-public-keys = [ "valeratrades.cachix.org-1:gXVwhzO5YB+BaiEJYT48qZgzdaErGQew6xtZcz4Fo1Q=" ];
  };

  inputs = {
    v_flakes.url = "github:valeratrades/v_flakes?ref=v1.6";
  };

  outputs = { self, v_flakes }:
    let
      # docs/port_range.md; this repository's ten are its first
      SA_PORT_RANGE = { from = 52800; to = 52900; };
      inherit (v_flakes) flake-utils pre-commit-hooks;
      manifest = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package;
      # The binary (crates/panel_server, `[[bin]] name`), the image and the release all go
      # by this name; the workspace root has no package of its own.
      pname = "panel";
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import v_flakes.default_nixpkgs { inherit system; };
        lib = pkgs.lib;
        rust = v_flakes.rs.default_nightly system;
        # mold is Linux-only — the adapter throws at *eval* time on Darwin, which
        # would break every target on a mac, not just the ones that link.
        stdenv = if pkgs.stdenv.hostPlatform.isDarwin then pkgs.stdenv else pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;

        # Single source for the container's exposed port and the prod bind.
        port = 59120;

        local = n: toString (SA_PORT_RANGE.from + n);
        localEnv = {
          SA_PORT_RANGE = "${toString SA_PORT_RANGE.from}-${toString SA_PORT_RANGE.to}";
          PANEL_BIND = "127.0.0.1:${local 0}";
          PANEL_WEB_PORT = local 1; # the front end's `npm run dev`
          STUB_PORT = local 2; # its `npm run dev:stub`
        };

        # `nix run .#gen`: the front end's mirror of the Rust (crates/panel_gen), committed.
        gen = pkgs.writeShellApplication {
          name = "gen";
          runtimeInputs = [ rust pkgs.git ];
          text = ''
            cd "$(git rev-parse --show-toplevel)"
            exec cargo run -q -p panel_gen "$@"
          '';
        };

        pre-commit-check = pre-commit-hooks.lib.${system}.run (
          let base = v_flakes.files.preCommit { inherit pkgs; stripClaudeSignature = true; }; in
          base // {
            hooks = base.hooks // {
              # Regenerate and re-stage rather than assert: a stale mirror type-checks against
              # itself and fails at runtime, in the browser.
              generated = {
                enable = true;
                name = "regenerate derived files";
                entry = "bash -c '${gen}/bin/gen && git add -A frontend/src/entities/session/model/generated.ts frontend/src/entities/source/model/generated.ts frontend/src/entities/gmb/model/generated.ts frontend/src/entities/tokens/model/generated.ts frontend/src/entities/alerts/model/generated.ts'";
                pass_filenames = false;
                require_serial = true;
              };
            };
          }
        );
        rs = v_flakes.rs {
          inherit pkgs rust;
          build.workspace = {
            "./crates/panel_server" = [ "git_version" "log_directives" ];
          };
        };
        github = v_flakes.github {
          inherit pkgs pname rs;
          enable = true;
          lastSupportedVersion = "nightly-2026-09-03";
          containerRelease = { registry = "ghcr.io/ev-service-arb"; };
          jobs.default = true;
          lfs = false;
        };
        readme = v_flakes.readme-fw {
          inherit pkgs pname;
          defaults = true;
          lastSupportedVersion = "nightly-1.100";
          rootDir = ./.;
          badges = [ "msrv" "loc" "ci" ];
        };
        combined = v_flakes.utils.combine { inherit rust; modules = [ rs github readme ]; };

        build_rust = v_flakes.rs.build_nightly system;
        rustPlatform = pkgs.makeRustPlatform { rustc = build_rust; cargo = build_rust; inherit stdenv; };
        # `.cargo` holds dev-only accelerators (sccache rustc-wrapper, cranelift,
        # mold) the hermetic sandbox lacks — drop it so the pure build uses nix's
        # own toolchain instead of failing on a missing `sccache` on PATH.
        pureSrc = lib.cleanSourceWith {
          src = lib.cleanSource ./.;
          filter = path: _type: baseNameOf path != ".cargo";
        };

        bin = rustPlatform.buildRustPackage {
          inherit pname;
          version = manifest.version;
          src = pureSrc;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "panel_server" ];
          nativeBuildInputs = with pkgs; [ pkg-config ];
          # ev_lib's `sentry` turns on reqwest's native-tls, which is OpenSSL on Linux
          # (Security.framework on Darwin, which needs nothing here).
          buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.openssl ];
          # `cargo test` runs in the devShell and CI; the tests bind loopback servers (fake
          # Telegram, PostHog and concierge), which the Darwin sandbox refuses unless asked
          doCheck = false;
          auditable = false; # cargo-auditable doesn't support edition 2024
          meta.description = "the panel binary";
        };

        # The front end's static export (`next build` → out/), served by `panel serve` from
        # PANEL_WEB_DIR on the same origin as /api and /auth.
        frontend = pkgs.buildNpmPackage {
          pname = "${pname}-frontend";
          version = manifest.version;
          src = lib.cleanSourceWith {
            src = lib.cleanSource ./frontend;
            filter = path: _type: !(builtins.elem (baseNameOf path) [ "node_modules" ".next" "out" ]);
          };
          npmDepsHash = "sha256-JKDCZ8uMBXRA/NxdpkIA5wtJyzr5zRoxhB4zynwrzZ0=";
          env = {
            NEXT_TELEMETRY_DISABLED = "1";
            # NEXT_PUBLIC_* values are inlined by `next build`, so these are compile-time:
            # the image carries the choice, no runtime switch. They turn on the platform path:
            # the sign-in dialog over the panel, the platform's account chip and sign-in, and
            # the wallet page. The bundles come from the platform origin (concierge's from
            # /api/auth/mfe/*, the wallet's from /wallet/mfe/*), which the server's CSP names
            # only with PANEL_CSP_PLATFORM_MFE=1 (devops panelEnv). Rollback: run the v0.12.1
            # image. v0.11.0 boots too, but it loads the chip through the pinned proxy, so
            # without its pins in the environment the chip shows only its fallback.
            NEXT_PUBLIC_SIGNIN_OVERLAY = "1";
            NEXT_PUBLIC_WALLET_PAGE = "1";
          };
          # Turbopack talks to its node workers over a loopback socket, which the Darwin
          # sandbox refuses unless asked.
          __darwinAllowLocalNetworking = true;
          # Turbopack's build stalls in the sandbox after "Compiled successfully" (its
          # PostCSS worker never hands back); webpack makes the same export.
          npmBuildFlags = [ "--" "--webpack" ];
          # next writes its cache under HOME, which the sandbox does not have
          preBuild = ''
            export HOME="$TMPDIR"
          '';
          installPhase = ''
            runHook preInstall
            cp -r out "$out"
            runHook postInstall
          '';
          meta.description = "the front end's static export";
        };

        # Grafana (docs/ARCHITECTURE.md, Grafana): the tilers plugin lays out the services' boards
        tilersPlugin = pkgs.fetchzip {
          url = "https://github.com/EV-Main/tilers/releases/download/grafana-v0.6.0/evmain-tilers-app-0.6.0.zip";
          hash = "sha256-NDxPRBj29ZmAb5ROp9SxPCSvrG0C+CVDWYPljcBeW4k=";
        };

        # Infinity's package for this platform alone: the catalog's default zip carries every platform's backend
        infinityPlugin = pkgs.grafanaPlugins.grafanaPlugin {
          pname = "yesoreyeram-infinity-datasource";
          version = "4.1.0";
          zipHash = {
            x86_64-linux = "sha256-pzgHZ2abzFzyKvBIQOTzlirus+AUTE0/VfP2zYnL06I=";
            aarch64-linux = "sha256-1fZ7v7mkleROOessTSbLB6wQH2HLMbhJdVGiqaaC/pg=";
          };
        };

        # Grafana without what no browser here loads: its source maps and API explorer
        grafanaPruned = pkgs.runCommand "grafana-pruned" { } ''
          cp -r --no-preserve=mode ${pkgs.grafana} $out
          chmod +x $out/bin/grafana
          find $out/share/grafana/public -name '*.map' -delete
          rm -r $out/share/grafana/public/build-swagger
        '';

        # PANEL_GRAFANA_SERVER: `serve` gives it its socket, data dir, provisioning, root URL and sign-in
        # settings; the rest is fixed here, cut to what the dashboards use (docs/ARCHITECTURE.md, Deploy
        # requirements). Warnings and up on serve's stderr.
        grafanaServer = pkgs.writeShellScript "grafana-server" ''
          export GF_PATHS_LOGS="$GF_PATHS_DATA/log"
          export GF_PATHS_PLUGINS="${pkgs.linkFarm "grafana-plugins" [
            { name = "yesoreyeram-infinity-datasource"; path = infinityPlugin; }
            { name = "marcusolsson-dynamictext-panel"; path = pkgs.grafanaPlugins.marcusolsson-dynamictext-panel; }
            { name = "evmain-tilers-app"; path = tilersPlugin; }
          ]}"
          export GF_PLUGINS_ALLOW_LOADING_UNSIGNED_PLUGINS=evmain-tilers-app
          export GF_SECURITY_ALLOW_EMBEDDING=true
          export GF_PLUGINS_PREINSTALL_DISABLED=true # its plugin dir is the store's
          export GF_LIVE_MAX_CONNECTIONS=0
          export GF_ANALYTICS_REPORTING_ENABLED=false GF_ANALYTICS_CHECK_FOR_UPDATES=false GF_ANALYTICS_CHECK_FOR_PLUGIN_UPDATES=false
          export GF_UNIFIED_ALERTING_ENABLED=false GF_NEWS_NEWS_FEED_ENABLED=false GF_PUBLIC_DASHBOARDS_ENABLED=false GF_SNAPSHOTS_ENABLED=false
          export GF_QUERY_HISTORY_ENABLED=false GF_EXPLORE_ENABLED=false GF_CLOUD_MIGRATION_ENABLED=false GF_RECORDED_QUERIES_ENABLED=false
          export GF_PROFILE_ENABLED=false GF_HELP_ENABLED=false
          # its own, not the pod's; Infinity inherits both.
          # The heap is kept by GOGC: a limit near the live heap (~70 MiB idle, ~105 after a while of loads) spins the GC
          export GOGC=50 GOMEMLIMIT=200MiB
          export GF_LOG_MODE=console GF_LOG_LEVEL=warn
          exec ${grafanaPruned}/bin/grafana server --homepath ${grafanaPruned}/share/grafana
        '';

        # The database: one SQLite file on the pod's volume, created and migrated by `serve` on
        # start (no init container, no migrate step), replicated off the pod by the cluster's
        # litestream — which is what `sqlite` below asks for. One writer: one pod, replaced
        # (Recreate), never two at once on the volume.
        dbPath = "/data/panel.db";

        # What the deploy must provide, beyond what `container.implement` records. Plain
        # data on the contract, read by the devops generator; `checks.contract-env` holds
        # `requiredEnv` to the binary's own `--print-required-vars`.
        deploy = {
          # Settings::required_var_names("production"): the pod exits 78 without any of them.
          # PANEL_DB_PATH is the container's own env (below); the rest the deploy supplies.
          requiredEnv = [ "PANEL_DB_PATH" "PANEL_DATA_KEY" "PANEL_PUBLIC_ORIGIN" "CONCIERGE_PUBLIC_ORIGIN" "CONCIERGE_GRPC_ADDR" "RP_CLIENT_SECRET_SA" "PANEL_ASSERTION_KEY" "PANEL_REVIEW_ARCHIVE_URL" "PANEL_PLAYBOOK_URL" "PANEL_SITES_URL" "TOKENS_START_CAP_MULTIPLE" ];
          # from the sops-backed Secret, never literal env
          secretEnv = [
            "PANEL_DATA_KEY"
            "SENTRY_DSN"
            "RP_CLIENT_SECRET_SA"
            "PANEL_ASSERTION_KEY"
            "TELEGRAM_BOT_TOKEN"
            "GOOGLE_MAPS_KEY"
          ];
          # PANEL_CSP_PLATFORM_MFE: `0` (default) or `1`. With `1` the pages' CSP also allows the
          # platform's MFE directories (https://evinvest.ltd/api/auth/mfe/ and /wallet/mfe/) in
          # script-src and the origin in connect-src. The front end always loads them from there,
          # so production sets `1`. TELEGRAM_BOT_TOKEN needs the forward (it sends sites' notes).
          optionalEnv = [
            "PANEL_CSP_PLATFORM_MFE"
            "SENTRY_DSN"
            "TELEGRAM_BOT_TOKEN"
            "TELEGRAM_BOT_USERNAME"
            "TELEGRAM_LOCALE"
            # 15 and 500 by default
            "TOKENS_DAILY"
            "TOKENS_CAP"
            # without it a GMB is added by its place id alone, unnamed
            "GOOGLE_MAPS_KEY"
          ];
          ingress = {
            # in-cluster only, by service DNS: the landings' and bots' ingest
            # (docs/ARCHITECTURE.md, Deploy requirements)
            excludePathPrefixes = [ "/api/ingest" ];
            # per client IP at the edge; the conductor bounds concurrency, not who calls. /api/hooks:
            # the booking providers' webhooks, forwarded to sites (public, signature-checked there)
            rateLimitPathPrefixes = [ "/auth" "/api/hooks" ];
          };
          egress = {
            # concierge's gRPC, at the address it names
            grpcEnv = [ "CONCIERGE_GRPC_ADDR" ];
            # the Bot API; the Places API (GOOGLE_MAPS_KEY)
            hosts = [ "api.telegram.org:443" "places.googleapis.com:443" ];
          };
        };

        # What the conductor serves to its peers, by endpoint: devops builds a consumer's URL from it.
        # `checks.provided-paths` holds it to the routes the binary mounts.
        provides.ingest = "/api/ingest/v1/events";
        # The services the forward reaches (docs/ARCHITECTURE.md, Forward), by contract name: devops
        # sets each variable to the peer's in-cluster origin and opens the egress to it.
        consumes = {
          PANEL_SITES_URL = "sites_panel";
          PANEL_REVIEW_ARCHIVE_URL = "review_archive";
          PANEL_PLAYBOOK_URL = "playbook_web";
        };

        containerStd = v_flakes.container.implement {
          inherit pkgs pname;
          containers."" = {
            inherit port;
            mounts = [ "/data" ];
            sqlite = [ dbPath ];
            # Grafana's own sqlite: its state is provisioned again on every start
            unreplicated = [ "/data/grafana" ];
            healthPath = "/health";
            # a source that gets a 5xx retries from its outbox; nothing is lost while down
            criticality = "normal";
            entrypoint = [ "${bin}/bin/${pname}" "serve" "--bind" "0.0.0.0:${toString port}" ];
            # /bin/panel, for `kubectl exec … panel source add` and the like
            contents = [ bin ];
            # the production guards (`#[required_in("production")]`) are armed wherever it runs
            env = {
              APP_ENV = "production";
              PANEL_DB_PATH = dbPath;
              PANEL_GRAFANA_DIR = "/data/grafana";
            };
            # the epoch versions the sa catalog `serve` publishes to concierge: it must travel with
            # the build, or a tag whose catalog changed boots on the previous epoch and is refused
            imageEnv = [ "PANEL_WEB_DIR=${frontend}" "PANEL_GRAFANA_SERVER=${grafanaServer}" "PANEL_BUILD_EPOCH=${toString self.lastModified}" ];
          };
        };
        # `implement` knows no key for the above, and throws on one it does not know.
        containers = lib.mapAttrs (_: c: c // { contract = c.contract // deploy // { inherit provides consumes; }; }) containerStd.containers;

        contractEnv = pkgs.runCommand "${pname}-contract-env" { } ''
          ${bin}/bin/${pname} --print-required-vars production | sort > got
          printf '%s\n' ${lib.escapeShellArgs deploy.requiredEnv} | sort > want
          diff -u want got || { echo "flake.nix deploy.requiredEnv disagrees with the binary" >&2; exit 1; }
          touch "$out"
        '';

        providedPaths = pkgs.runCommand "${pname}-provided-paths" { } ''
          ${bin}/bin/${pname} --print-provided-paths | sort > got
          printf '%s\n' ${lib.escapeShellArgs (lib.attrValues provides)} | sort > want
          diff -u want got || { echo "flake.nix provides disagrees with the routes the binary mounts" >&2; exit 1; }
          touch "$out"
        '';

        # The conductor (dev sign-in), sites_panel and the landings beside its checkout, wired
        # together on this machine; the script is the source, shellchecked here (docs/LOCAL.md). It builds the
        # binary and the front end itself, unless PANEL_BIN / PANEL_WEB_DIR name others: the
        # app does not depend on them, so naming a cargo build skips the Nix one.
        localStack = pkgs.writeShellApplication {
          name = "local-stack";
          runtimeInputs = with pkgs; [ coreutils curl gawk git gnused util-linux nodejs_22 watchexec litestream sops yq-go openssh gnutar ];
          text = lib.concatStrings (lib.mapAttrsToList (k: v: "export ${k}=${v}\n") localEnv) + builtins.readFile ./scripts/local-stack.sh;
        };

        # `nix run .#dev`'s peers, from ../<name> unless their user's entry names a `path` or `command` (docs/LOCAL.md)
        devPeers = [ "sites_panel" "review_archive" ];
        devUsers = {
          v.sites_panel.path = "../sites/panel";
        };
        devPrelude = lib.concatStrings (lib.mapAttrsToList
          (user: peers:
            "if [ \"$(id -un)\" = ${lib.escapeShellArg user} ]; then\n"
            + lib.concatStrings (lib.mapAttrsToList
              (peer: o:
                assert lib.assertMsg (builtins.elem peer devPeers) "devUsers.${user}.${peer}: not one of devPeers";
                assert lib.assertMsg (lib.subtractLists [ "path" "command" ] (lib.attrNames o) == [ ]) "devUsers.${user}.${peer}: only path and command";
                lib.concatStrings (lib.mapAttrsToList (k: v: "export DEV_${lib.toUpper k}_${peer}=${lib.escapeShellArg v}\n") o))
              peers)
            + "fi\n")
          devUsers);
        dev = pkgs.writeShellApplication {
          name = "dev";
          runtimeInputs = [ pkgs.coreutils ];
          text = devPrelude + ''exec ${lib.getExe localStack} --dev "$@"'';
        };
      in
      {
        apps = {
          help = v_flakes.utils.help {
            inherit pkgs self system;
            about = "  cargo test                      every test; the database ones on throwaway SQLite files\n"
              + "  panel --help                    the CLI itself";
          };
          publish = v_flakes.utils.publish pkgs;
          gen = { type = "app"; program = lib.getExe gen; meta.description = "the front end's mirror of the Rust types (pre-commit runs it too)"; };
          dev = { type = "app"; program = lib.getExe dev; meta.description = "the conductor and its peers under watch, each started unless it already serves (docs/LOCAL.md)"; };
          local-stack = { type = "app"; program = lib.getExe localStack; meta.description = "the conductor, its Grafana, sites_panel, review_archive and both landings, wired together on this machine (docs/LOCAL.md)"; };
        };

        packages = {
          default = bin;
          inherit bin frontend;
        } // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (containerStd.packages // { grafana-server = grafanaServer; });

        containers = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux containers;

        checks.contract-env = contractEnv;
        checks.provided-paths = providedPaths;

        devShells.default =
          with pkgs;
          mkShell {
            inherit stdenv;
            shellHook =
              pre-commit-check.shellHook
              + combined.shellHook
              + ''
                cp -f ${(v_flakes.files.treefmt) { inherit pkgs; }} ./.treefmt.toml
                cp -f ${(v_flakes.files.gitattributes) { inherit pkgs; lfs = false; }} ./.gitattributes
              '';

            packages = [
              mold
              pkg-config
              openssl # sentry's native-tls
              sqlite # inspecting the database
              rust
            ] ++ pre-commit-check.enabledPackages ++ combined.enabledPackages;

            env = localEnv // {
              RUST_BACKTRACE = 1;
              RUST_LIB_BACKTRACE = 0;
            };
          };
      }
    );
}
