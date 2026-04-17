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
        name = "onnxruntime-prebuilt-1.23.2";
        src = pkgs.fetchurl {
          url = "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.23.2/x86_64-unknown-linux-gnu.tar.lzma2";
          hash = "sha256-jFfQWaqu5AeBKlaY1nBseeCQrWnhoUIEMJ6ALcu6o18=";
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

      # Package builder — reusable across overlay and standalone packages
      mkPkg = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "pares-agens";
        version = "0.6.1";
        src = pkgs.lib.cleanSource ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
          allowBuiltinFetchGit = true;
        };

        cargoBuildFlags = [ "-p" "pares-agens" ];

        nativeBuildInputs = with pkgs; [ pkg-config cmake ];
        buildInputs = with pkgs; [ openssl stdenv.cc.cc.lib ];

        # Point ort-sys to prefetched ONNX Runtime (pure sandbox, no network)
        ORT_LIB_LOCATION = "${onnxruntimeLib { inherit pkgs; }}/lib";

        # fastembed downloads ONNX model at first run, not build time
        FASTEMBED_CACHE_PATH = "/tmp/fastembed-cache";

        meta = {
          description = "Native AI agent framework — 3-consciousness architecture on PluresDB";
          homepage = "https://github.com/plures/pares-agens";
          license = pkgs.lib.licenses.bsl11;
          mainProgram = "pares-agens";
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

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust pkg-config openssl cmake stdenv.cc.cc.lib cargo-watch
          ];
        };
      }
    ) // {
      # Overlay — builds pares-agens with the CONSUMER's pkgs (inherits allowUnfree)
      overlays.default = final: prev: {
        pares-agens = mkPkg final;
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
                Type = "simple";
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
                  telegramArg = if cfg.telegramTokenFile != null
                    then "--telegram-token $(cat ${cfg.telegramTokenFile})"
                    else "";
                  braveArg = if cfg.braveApiKeyFile != null
                    then "--brave-api-key $(cat ${cfg.braveApiKeyFile})"
                    else "";
                  copilotArg = if cfg.copilot then "--copilot" else "";
                  modelArg = "--model ${cfg.model} --deep-model ${cfg.deepModel}";
                  promptArg = if cfg.systemPromptFile != null
                    then "--system-prompt ${cfg.systemPromptFile}"
                    else "";
                  extraArgs = lib.concatStringsSep " " cfg.extraFlags;
                in
                ''
                  exec ${cfg.package}/bin/pares-agens serve \
                    ${copilotArg} \
                    ${telegramArg} \
                    ${braveArg} \
                    ${modelArg} \
                    ${promptArg} \
                    ${extraArgs}
                '';
            };
          };
        };
    };
}
