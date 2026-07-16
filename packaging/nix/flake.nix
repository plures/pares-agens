{
  description = "Pares Agens — local-first AI agent desktop (end-user AppImage package)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAll = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAll (system:
        let pkgs = import nixpkgs { inherit system; config.allowUnfree = true; };
        in {
          pares-agens = import ./default.nix { inherit pkgs; };
          default = self.packages.${system}.pares-agens;
        });

      apps = forAll (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.pares-agens}/bin/pares-agens";
        };
      });

      # Overlay so downstream flakes/configs can `pkgs.pares-agens`.
      overlays.default = final: prev: {
        pares-agens = import ./default.nix { pkgs = final; };
      };
    };
}
