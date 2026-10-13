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
        stdenv = if pkgs.stdenv.isDarwin then pkgs.stdenv else pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;

        # Single source for the container's exposed port and the prod bind. The binary's
        # own default (`panel_server::DEFAULT_BIND`) is the same port on 127.0.0.1.
        port = 59120;

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
                entry = "bash -c '${gen}/bin/gen && git add -A frontend/src/entities/session/model/generated.ts frontend/src/entities/lead/model/generated.ts frontend/src/entities/source/model/generated.ts frontend/src/shared/config/generated.ts frontend/src/shared/api/generated.ts'";
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
          buildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.openssl ];
          # `cargo test` runs in the devShell and CI; the tests bind loopback servers (fake
          # Telegram, PostHog and concierge), which the Darwin sandbox refuses unless asked
          doCheck = false;
          auditable = false; # cargo-auditable doesn't support edition 2024
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
          npmDepsHash = "sha256-GSfZ3zYqm+A58xzhZiTGxOsxQoL9lSbg1Ra4l5LrfeI=";
          env = {
            NEXT_TELEMETRY_DISABLED = "1";
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
        };

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
          requiredEnv = [ "PANEL_DB_PATH" "PANEL_DATA_KEY" "PANEL_PUBLIC_ORIGIN" "CONCIERGE_PUBLIC_ORIGIN" "CONCIERGE_GRPC_ADDR" "RP_CLIENT_SECRET_SA" "PANEL_ASSERTION_KEY" "PANEL_REVIEW_ARCHIVE_URL" "PANEL_PLAYBOOK_URL" ];
          # from the sops-backed Secret, never literal env
          secretEnv = [
            "PANEL_DATA_KEY"
            "SENTRY_DSN"
            "RP_CLIENT_SECRET_SA"
            "PANEL_ASSERTION_KEY"
            "TELEGRAM_BOT_TOKEN"
            "GOOGLE_OAUTH_CLIENT_SECRET"
            "GOOGLE_CALENDAR_REFRESH_TOKEN_AQUAFIX"
            "GOOGLE_CALENDAR_REFRESH_TOKEN_VIFNET"
          ];
          # POSTHOG_PROJECT_API_KEY (phc_, public like the landings', not a secret; + POSTHOG_HOST):
          # serve sends the leads' life to PostHog; unset, nothing is sent (serve warns).
          # POSTHOG_PROJECT_ID (+ POSTHOG_APP_HOST): the experiments link to their funnels in
          # PostHog; unset, no links. Links are opened by the browser: no egress for them.
          # Google Calendar booking pull (docs/ARCHITECTURE.md, Booking): the OAuth client both or
          # neither; a brand's calendar is pulled only with its refresh token
          # (`panel booking google-authorize <brand>` makes one); its calendar id defaults to
          # `primary`. A token without the client fails the boot.
          # PANEL_CABINET_URL: the cabinet's bare http origin; /cabinet/mfe/* (the account chip's
          # bundle) is served from it, and only with PANEL_CABINET_MFE_PINS
          # (`account-chip.js=<sha256 hex>,account-chip.css=<sha256 hex>`, banking's
          # `npm run mfe:pins`; public, no egress). Either unset, the route is not mounted and
          # answers JSON 404; malformed pins fail the boot.
          optionalEnv = [
            "PANEL_CABINET_URL"
            "PANEL_CABINET_MFE_PINS"
            "SENTRY_DSN"
            "TELEGRAM_BOT_TOKEN"
            "TELEGRAM_BOT_USERNAME"
            "TELEGRAM_LOCALE"
            "POSTHOG_PROJECT_API_KEY"
            "POSTHOG_HOST"
            "POSTHOG_PROJECT_ID"
            "POSTHOG_APP_HOST"
            "GOOGLE_OAUTH_CLIENT_ID"
            "GOOGLE_OAUTH_CLIENT_SECRET"
            "GOOGLE_CALENDAR_SYNC_MINUTES"
            "GOOGLE_CALENDAR_REFRESH_TOKEN_AQUAFIX"
            "GOOGLE_CALENDAR_ID_AQUAFIX"
            "GOOGLE_CALENDAR_REFRESH_TOKEN_VIFNET"
            "GOOGLE_CALENDAR_ID_VIFNET"
          ];
          ingress = {
            # in-cluster only, by service DNS: the landings' ingest and place reads
            # (docs/ARCHITECTURE.md, Deploy requirements)
            excludePathPrefixes = [ "/api/ingest" "/api/internal" ];
            # per client IP at the edge; the panel bounds concurrency, not who calls. /api/hooks:
            # the booking providers' webhooks (public, signature-checked; none registered yet)
            rateLimitPathPrefixes = [ "/auth" "/api/hooks" ];
          };
          egress = {
            # in-cluster peers, at the addresses these name: concierge's gRPC, and the services
            # the forward reaches (docs/ARCHITECTURE.md, Forward)
            grpcEnv = [ "CONCIERGE_GRPC_ADDR" "PANEL_REVIEW_ARCHIVE_URL" "PANEL_PLAYBOOK_URL" "PANEL_CABINET_URL" ];
            # the Bot API; PostHog's capture host (POSTHOG_HOST's default — follow it if it is
            # pointed elsewhere); Google's token endpoint and Calendar API for the booking pull
            hosts = [ "api.telegram.org:443" "us.i.posthog.com:443" "oauth2.googleapis.com:443" "www.googleapis.com:443" ];
          };
        };

        containerStd = v_flakes.container.implement {
          inherit pkgs pname;
          containers."" = {
            inherit port;
            mounts = [ "/data" ];
            sqlite = [ dbPath ];
            healthPath = "/health";
            # a source that gets a 5xx retries from its outbox; nothing is lost while down
            criticality = "normal";
            entrypoint = [ "${bin}/bin/${pname}" "serve" "--bind" "0.0.0.0:${toString port}" ];
            # /bin/panel, for `kubectl exec … panel source add` and the like
            contents = [ bin ];
            # the production guards (`#[required_in("production")]`) are armed wherever it runs
            # PostHog: the project the landings send to (Cloud US 614067, their
            # deploy/config.nix); the phc_ token is a public write-only ingest key
            env = {
              APP_ENV = "production";
              PANEL_DB_PATH = dbPath;
              POSTHOG_PROJECT_API_KEY = "phc_sBwWEgdgockVmfyucBRkTTo6iZ4Y2eApSGorD22WLzj3";
              POSTHOG_PROJECT_ID = "614067";
              # the version of the sa catalog `serve` publishes to concierge
              PANEL_BUILD_EPOCH = toString self.lastModified;
            };
            imageEnv = [ "PANEL_WEB_DIR=${frontend}" ];
          };
        };
        # `implement` knows no key for the above, and throws on one it does not know.
        containers = lib.mapAttrs (_: c: c // { contract = c.contract // deploy; }) containerStd.containers;

        contractEnv = pkgs.runCommand "${pname}-contract-env" { } ''
          ${bin}/bin/${pname} --print-required-vars production | sort > got
          printf '%s\n' ${lib.escapeShellArgs deploy.requiredEnv} | sort > want
          diff -u want got || { echo "flake.nix deploy.requiredEnv disagrees with the binary" >&2; exit 1; }
          touch "$out"
        '';

        help = pkgs.writeShellApplication {
          name = "help";
          text = ''
            cat <<'EOF'
            nix develop                       toolchain, sqlite
            nix build                         the panel binary
            nix build .#${pname}-container    OCI image, the front end in it (Linux only)
            nix build .#frontend              the front end's static export
            nix run .#local-stack             the panel and both landings on this machine,
                                              wired together (docs/LOCAL.md)
            nix run .#publish -- major|minor|patch [note]
                                              tag the next vX.Y.Z and push it: CI ships the image
            nix run .#gen                     the front end's mirror of the Rust types
                                              (pre-commit runs it too)
            nix run .#help                    this
            cargo test                        every test; the database ones on throwaway
                                              SQLite files, nothing to set up
            the CLI itself: panel --help
            EOF
          '';
        };
        # The version lives in the tag, not in Cargo.toml: `release-container.yml` ships the image on it.
        publish = pkgs.writeShellApplication {
          name = "publish";
          runtimeInputs = with pkgs; [ git ];
          text = ''
            part="''${1:-}"
            case "$part" in
              major | minor | patch) ;;
              *) echo "usage: nix run .#publish -- major|minor|patch [release note]" >&2; exit 1 ;;
            esac
            [ -z "$(git status --porcelain)" ] || { echo "uncommitted changes — commit or stash first" >&2; exit 1; }
            shift
            note="$*"

            git fetch --tags --force origin >/dev/null # not silenced: a stale tag list reuses a version
            last="$(git tag -l 'v*' --sort=-v:refname | head -n1)"
            ver="''${last#v}"; [ -n "$ver" ] || ver="0.0.0"
            ma="''${ver%%.*}"; rest="''${ver#*.}"; mi="''${rest%%.*}"; pa="''${rest##*.}"
            case "$part" in
              major) ma=$((ma + 1)); mi=0; pa=0 ;;
              minor) mi=$((mi + 1)); pa=0 ;;
              patch) pa=$((pa + 1)) ;;
            esac
            next="v$ma.$mi.$pa"
            echo "''${last:-<no tag>} → $next"

            # annotated: a lightweight tag has nowhere to put a note, and `--follow-tags` skips it
            if [ -n "$note" ]; then
              printf '%s\n\n%s\n' "$next" "$note" | git tag -a "$next" -F -
            else
              git tag -a "$next" -m "$next"
            fi
            git push origin "$next"
          '';
        };
        # The panel (dev sign-in) and the landings beside its checkout, wired together on this
        # machine; the script is the source, shellchecked here (docs/LOCAL.md). It builds the
        # binary and the front end itself, unless PANEL_BIN / PANEL_WEB_DIR name others: the
        # app does not depend on them, so naming a cargo build skips the Nix one.
        localStack = pkgs.writeShellApplication {
          name = "local-stack";
          runtimeInputs = with pkgs; [ coreutils curl gawk git gnused ];
          text = builtins.readFile ./scripts/local-stack.sh;
        };
      in
      {
        apps = {
          help = { type = "app"; program = lib.getExe help; };
          publish = { type = "app"; program = lib.getExe publish; };
          gen = { type = "app"; program = lib.getExe gen; };
          local-stack = { type = "app"; program = lib.getExe localStack; };
        };

        packages = {
          default = bin;
          inherit bin frontend;
        } // lib.optionalAttrs pkgs.stdenv.isLinux containerStd.packages;

        containers = lib.optionalAttrs pkgs.stdenv.isLinux containers;

        checks.contract-env = contractEnv;

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

            env = {
              RUST_BACKTRACE = 1;
              RUST_LIB_BACKTRACE = 0;
            };
          };
      }
    );
}
