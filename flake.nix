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
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" ];
        };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "pares-agens";
          version = "0.6.1";
          src = pkgs.lib.cleanSource ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            # PluresDB is fetched from GitHub — allow network access for git deps
            allowBuiltinFetchGit = true;
          };

          # Only build the serve binary (crates/migrate = pares-agens CLI)
          cargoBuildFlags = [ "-p" "pares-agens" ];

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake       # for onnxruntime/fastembed build
          ];

          buildInputs = with pkgs; [
            openssl
            stdenv.cc.cc.lib  # libstdc++ for fastembed/ONNX
          ];

          # fastembed downloads ONNX model at build time — allow network
          FASTEMBED_CACHE_PATH = "/tmp/fastembed-cache";

          meta = {
            description = "Native AI agent framework — 3-consciousness architecture on PluresDB";
            homepage = "https://github.com/plures/pares-agens";
            license = {
              spdxId = "BSL-1.1";
              fullName = "Business Source License 1.1";
              url = "https://mariadb.com/bsl11/";
              free = true;  # BSL converts to open source after change date
            };
            mainProgram = "pares-agens";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rust
            pkg-config
            openssl
            cmake
            stdenv.cc.cc.lib
            cargo-watch
          ];
        };
      }
    ) // {
      # NixOS module for running pares-agens as a systemd service
      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.pares-agens;
        in
        {
          options.services.pares-agens = {
            enable = lib.mkEnableOption "Pares Agens AI agent daemon";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "The pares-agens package to use.";
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
              description = "Whether to create the service user. Set false when using an existing user (e.g. kbristol).";
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
                # Hardening (relaxed when running as existing user)
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
