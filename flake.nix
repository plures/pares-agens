{
  description = "Pares Agens — native AI agent framework on the plures stack";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    let
      # Prefetch ONNX Runtime static library for ort-sys.
      # ort-sys downloads this at build time from cdn.pyke.io in a custom lzma2
      # tar format. We prefetch + extract it so the Nix sandbox stays pure.
      onnxruntimeLib = { pkgs }: pkgs.stdenvNoCC.mkDerivation {
        name = "onnxruntime-prebuilt-1.24.2";
        src = pkgs.fetchurl {
          url = "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/x86_64-unknown-linux-gnu.tar.lzma2";
          hash = "sha256-rMHLp5wzdZTq0diMpyUWFHqmAFTIQhe1M5mjHKpbpnE=";
        };
        nativeBuildInputs = [ pkgs.python3 ];
        dontUnpack = true;
        installPhase = ''
          mkdir -p $out/lib
          python3 -c "
import lzma, tarfile, io, sys, os
with open(sys.argv[1], 'rb') as f:
    raw = f.read()
data = lzma.decompress(raw, format=lzma.FORMAT_RAW, filters=[{'id': lzma.FILTER_LZMA2, 'dict_size': 1 << 26}])
tar = tarfile.open(fileobj=io.BytesIO(data))
tar.extractall(os.environ['out'] + '/lib')
" $src
        '';
      };

      # Vendored BGE small embedding model (Xenova/bge-small-en-v1.5) so the
      # hermetic Nix build/check can run BgeLocalEmbedder fully offline.
      # fastembed's EmbeddingModel::BGESmallENV15 maps to this repo/model_file.
      bgeModelRepo = "Xenova/bge-small-en-v1.5";
      # Pinned HF commit these files were vendored from (verified via refs/main
      # produced by an actual fastembed download during development).
      bgeModelRevision = "ea104dacec62c0de699686887e3f920caeb4f3e3";

      bgeFastembedCache = { pkgs }:
        let
          bgeConfig = pkgs.fetchurl {
            url = "https://huggingface.co/${bgeModelRepo}/resolve/main/config.json";
            hash = "sha256-+nP5C/ksjKzh+8twliYwbyvbyeo+W1+UtEDfm2qlY1A=";
          };
          bgeTokenizer = pkgs.fetchurl {
            url = "https://huggingface.co/${bgeModelRepo}/resolve/main/tokenizer.json";
            hash = "sha256-0kGmDV6PBMwbKz6e96SSGye/Um2fYFCrkPkmeh+eXGY=";
          };
          bgeTokenizerConfig = pkgs.fetchurl {
            url = "https://huggingface.co/${bgeModelRepo}/resolve/main/tokenizer_config.json";
            hash = "sha256-kmHn15tEyBlcHK2itFPlWwCuuB6QemZkl0tNd3YXKrM=";
          };
          bgeSpecialTokens = pkgs.fetchurl {
            url = "https://huggingface.co/${bgeModelRepo}/resolve/main/special_tokens_map.json";
            hash = "sha256-ttNGvjZqfR1IMy28n987+JYLXYeVIrd5ndulnnYjfuM=";
          };
          bgeModelOnnx = pkgs.fetchurl {
            url = "https://huggingface.co/${bgeModelRepo}/resolve/main/onnx/model.onnx";
            hash = "sha256-go4Ultf6u3nPpNzYT6OGJcDT0h2kdKAPCNsPVZlAzzU=";
          };
        in
        # hf-hub's Cache::repo().get(filename) resolves via:
        #   refs/<revision>            -> commit hash string
        #   snapshots/<commit>/<file>  -> actual file content
        # Pin to the real upstream commit (verified by inspecting refs/main
        # produced by an actual fastembed/hf-hub download) so the cache layout
        # matches exactly what hf-hub would create itself.
        pkgs.runCommandNoCC "fastembed-bge-small-en-v1.5-cache" { } ''
          root="$out/models--Xenova--bge-small-en-v1.5"
          snapshot="$root/snapshots/${bgeModelRevision}"
          mkdir -p "$root/refs" "$snapshot/onnx"
          printf '%s' "${bgeModelRevision}" > "$root/refs/main"
          cp ${bgeConfig} "$snapshot/config.json"
          cp ${bgeTokenizer} "$snapshot/tokenizer.json"
          cp ${bgeTokenizerConfig} "$snapshot/tokenizer_config.json"
          cp ${bgeSpecialTokens} "$snapshot/special_tokens_map.json"
          cp ${bgeModelOnnx} "$snapshot/onnx/model.onnx"
        '';

      # Package builder — reusable across overlay and standalone packages
      mkPkg = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "pares-agens";
        version = "0.6.1";
        src = pkgs.lib.cleanSource ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
          allowBuiltinFetchGit = true;
        };

        # The packaged daemon is the spine host.  Its executable is renamed in
        # postInstall so consumers retain the stable `pares-agens` command
        # while receiving the PX-driven `serve-spine` surface.
        cargoBuildFlags = [ "-p" "agens-plugin" ];
        # Real (non-ignored) offline BGE embedder test lives behind the
        # `embeddings` feature on pares-agens-core; run it explicitly so the
        # hermetic build proves the vendored model works, not just skips it.
        cargoTestFlags = [ "-p" "pares-agens-core" "--features" "embeddings" ];

        nativeBuildInputs = with pkgs; [ pkg-config cmake ];
        buildInputs = with pkgs; [ openssl zlib stdenv.cc.cc.lib glib pango cairo gdk-pixbuf atk gtk3 graphene webkitgtk_4_1 libsoup_3 ];

        # Point ort-sys to prefetched ONNX Runtime (pure sandbox, no network)
        ORT_LIB_LOCATION = "${onnxruntimeLib { inherit pkgs; }}/lib";

        # Vendored BGE model cache, staged at the path fastembed's BgeLocalEmbedder
        # actually reads: fastembed's get_cache_dir() returns
        # $FASTEMBED_CACHE_DIR (falling back to "./.fastembed_cache" relative to
        # CWD), which it passes straight into hf_hub::ApiBuilder::with_cache_dir().
        # This is NOT the same as hf-hub's own Cache::default() ($HOME/.cache/
        # huggingface/hub) — verified empirically: fastembed always supplies an
        # explicit cache_dir, so hf-hub's built-in default/HF_HOME logic never
        # triggers here. cargo's checkPhase runs from the crate root, so pin
        # FASTEMBED_CACHE_DIR to an absolute writable path built from the
        # read-only vendored derivation.
        preCheck = ''
          export HOME="$TMPDIR/home"
          mkdir -p "$HOME"
          export FASTEMBED_CACHE_DIR="$TMPDIR/fastembed-cache"
          mkdir -p "$FASTEMBED_CACHE_DIR"
          cp -R ${bgeFastembedCache { inherit pkgs; }}/. "$FASTEMBED_CACHE_DIR/"
          chmod -R u+w "$FASTEMBED_CACHE_DIR"
        '';

        postInstall = ''
          mv "$out/bin/agens-host" "$out/bin/pares-agens"
          install -d "$out/share/pares-agens/praxis"
          cp -r praxis/procedures "$out/share/pares-agens/praxis/procedures"
        '';

        meta = {
          description = "Native AI agent framework — 3-consciousness architecture on PluresDB";
          homepage = "https://github.com/plures/pares-agens";
          license = pkgs.lib.licenses.bsl11;
          mainProgram = "pares-agens";
        };
      };

      # Package builder for the desktop (system tray) Tauri app — no npm step needed.
      mkDesktopPkg = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "pares-agens-desktop";
        version = "0.5.0";
        src = pkgs.lib.cleanSource ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
          allowBuiltinFetchGit = true;
        };

        cargoBuildFlags = [ "-p" "pares-agens-desktop" ];

        nativeBuildInputs = with pkgs; [ pkg-config cmake ];
        buildInputs = with pkgs; [
          openssl zlib stdenv.cc.cc.lib glib pango cairo gdk-pixbuf atk gtk3
          graphene webkitgtk_4_1 libsoup_3
        ];

        ORT_LIB_LOCATION = "${onnxruntimeLib { inherit pkgs; }}/lib";
        # Desktop package does not run the BGE embedder test in checkPhase;
        # correct env var name only (fastembed downloads lazily at runtime here).
        FASTEMBED_CACHE_DIR = "/tmp/fastembed-cache";

        meta = {
          description = "Pares Agens desktop app — system tray agent node";
          homepage = "https://github.com/plures/pares-agens";
          license = pkgs.lib.licenses.bsl11;
          mainProgram = "pares-agens-desktop";
        };
      };
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; config.allowUnfree = true; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" ];
        };
      in
      {
        packages.default = mkPkg pkgs;
        packages.pares-agens-desktop = mkDesktopPkg pkgs;

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust pkg-config openssl zlib cmake stdenv.cc.cc.lib cargo-watch
            glib pango cairo gdk-pixbuf atk gtk3 graphene
            webkitgtk_4_1 libsoup_3
          ];
          # zlib ships zlib.pc under $dev/share/pkgconfig (NOT lib/pkgconfig),
          # which mkShell's pkg-config hook does not add. gdk-3.0.pc requires
          # zlib, so expose it explicitly or gdk-sys fails to build.
          shellHook = ''
            export PKG_CONFIG_PATH="${pkgs.zlib.dev}/share/pkgconfig:${pkgs.zlib.dev}/lib/pkgconfig:$PKG_CONFIG_PATH"
          '';
        };
      }
    ) // {
      # Overlay — builds pares-agens with the CONSUMER's pkgs (inherits allowUnfree)
      overlays.default = final: prev: {
        pares-agens = mkPkg final;
        pares-agens-desktop = mkDesktopPkg final;
      };

      # NixOS module
      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.pares-agens;
        in
        {
          options.services.pares-agens = {
            enable = lib.mkEnableOption "Pares Agens AI agent daemon";

            package = lib.mkOption {
              type = lib.types.package;
              # Default uses pkgs.pares-agens from the overlay (consumer's pkgs)
              default = pkgs.pares-agens;
              defaultText = lib.literalExpression "pkgs.pares-agens";
              description = "The pares-agens package to use. Requires the pares-agens overlay.";
            };

            user = lib.mkOption {
              type = lib.types.str;
              default = "pares-agens";
              description = "User account under which the service runs.";
            };

            group = lib.mkOption {
              type = lib.types.str;
              default = "pares-agens";
              description = "Group under which the service runs.";
            };

            dataDir = lib.mkOption {
              type = lib.types.path;
              default = "/var/lib/pares-agens";
              description = "Directory for PluresDB storage and Copilot auth cache.";
            };

            copilot = lib.mkOption {
              type = lib.types.bool;
              default = true;
              description = "Use GitHub Copilot OAuth device flow for LLM access.";
            };

            model = lib.mkOption {
              type = lib.types.str;
              default = "gpt-4.1";
              description = "Conscious model (80% of traffic).";
            };

            deepModel = lib.mkOption {
              type = lib.types.str;
              default = "claude-opus-4.6";
              description = "Deep model for low-confidence escalation.";
            };

            telegramTokenFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = "Path to file containing the Telegram bot token.";
            };

            braveApiKeyFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = "Path to file containing the Brave Search API key.";
            };

            syncTopicKey = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              description = "32-byte Hyperswarm sync topic key in hex for multi-host memory replication.";
            };

            syncSharedKeyFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = "Path to file containing shared SEA key required for sync payload decryption.";
            };

            systemPromptFile = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              description = "Path to a system prompt file.";
            };

            createUser = lib.mkOption {
              type = lib.types.bool;
              default = true;
              description = "Whether to create the service user. Set false for existing users.";
            };

            extraFlags = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [];
              description = "Additional command-line flags.";
            };
          };

          config = lib.mkIf cfg.enable {
            assertions = [
              {
                assertion = cfg.telegramTokenFile != null;
                message = "services.pares-agens.telegramTokenFile must be set. pares-agens serve requires PARES_TELEGRAM_TOKEN.";
              }
              {
                assertion = cfg.syncTopicKey == null || cfg.syncSharedKeyFile != null;
                message = "services.pares-agens.syncSharedKeyFile must be set when services.pares-agens.syncTopicKey is configured.";
              }
            ];

            users.users.${cfg.user} = lib.mkIf cfg.createUser {
              isSystemUser = true;
              group = cfg.group;
              home = cfg.dataDir;
              createHome = true;
            };

            users.groups.${cfg.group} = lib.mkIf cfg.createUser {};

            systemd.services.pares-agens = {
              description = "Pares Agens — AI Agent Daemon";
              wantedBy = [ "multi-user.target" ];
              after = [ "network-online.target" ];
              wants = [ "network-online.target" ];

              environment = {
                RUST_LOG = "info";
                HOME = cfg.dataDir;
              };

              serviceConfig = {
                Type = "notify";
                NotifyAccess = "main";
                WatchdogSec = 30;
                User = cfg.user;
                Group = cfg.group;
                WorkingDirectory = cfg.dataDir;
                Restart = "on-failure";
                RestartSec = 10;
                NoNewPrivileges = lib.mkIf cfg.createUser true;
                ProtectSystem = lib.mkIf cfg.createUser "strict";
                ProtectHome = lib.mkIf cfg.createUser true;
                ReadWritePaths = [ cfg.dataDir ];
                PrivateTmp = true;
              };

              script =
                let
                  escapedTelegramTokenFile = lib.escapeShellArg (toString cfg.telegramTokenFile);
                  copilotArg = if cfg.copilot then "--copilot" else "";
                  modelArg = "--model ${cfg.model} --deep-model ${cfg.deepModel}";
                  promptArg = if cfg.systemPromptFile != null
                    then "--system-prompt ${cfg.systemPromptFile}"
                    else "";
                  syncArg = if cfg.syncTopicKey != null
                    then "--sync-topic-key ${cfg.syncTopicKey}"
                    else "";
                  escapedBraveApiKeyFile = if cfg.braveApiKeyFile != null
                    then lib.escapeShellArg (toString cfg.braveApiKeyFile)
                    else null;
                  escapedSyncSharedKeyFile = if cfg.syncSharedKeyFile != null
                    then lib.escapeShellArg (toString cfg.syncSharedKeyFile)
                    else null;
                  telegramTokenExport = "export PARES_TELEGRAM_TOKEN=\"$(tr -d '\\r\\n' < ${escapedTelegramTokenFile})\"";
                  braveApiKeyExport = if cfg.braveApiKeyFile != null
                    then "export BRAVE_API_KEY=\"$(tr -d '\\r\\n' < ${escapedBraveApiKeyFile})\""
                    else "";
                  syncSharedKeyExport = if cfg.syncSharedKeyFile != null
                    then "export PARES_SYNC_SHARED_KEY=\"$(tr -d '\\r\\n' < ${escapedSyncSharedKeyFile})\""
                    else "";
                  extraArgs = lib.concatStringsSep " " cfg.extraFlags;
                in
                ''
                  ${telegramTokenExport}
                  ${braveApiKeyExport}
                  ${syncSharedKeyExport}

                  exec ${cfg.package}/bin/pares-agens serve \
                    ${copilotArg} \
                    ${modelArg} \
                    ${promptArg} \
                    ${syncArg} \
                    ${extraArgs}
                '';
            };
          };
        };

      # NixOS module for the desktop (system tray) app — runs as a user service.
      nixosModules.desktop = { config, lib, pkgs, ... }:
        let
          cfg = config.services.pares-agens-desktop;
        in
        {
          options.services.pares-agens-desktop = {
            enable = lib.mkEnableOption "Pares Agens desktop system-tray agent";

            package = lib.mkOption {
              type = lib.types.package;
              default = pkgs.pares-agens-desktop;
              defaultText = lib.literalExpression "pkgs.pares-agens-desktop";
              description = "The pares-agens-desktop package to use.";
            };
          };

          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
          };
        };
    };
}
