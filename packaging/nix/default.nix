# Pares Agens — NixOS / Nix end-user package
#
# Installs the PUBLIC pre-built Linux AppImage from the GitHub Release using
# appimageTools.wrapType2. No source build of the private plures repos, no
# GitHub token, no private-flake dance — end users just download the released
# binary.
#
# To pin a new release:
#   1. set `version` to the release tag (without the leading "v").
#   2. run: nix-prefetch-url https://github.com/plures/pares-agens/releases/download/v<version>/pares-agens_amd64.AppImage
#   3. paste the resulting hash into `src.sha256`.

{ pkgs ? import <nixpkgs> { } }:

let
  version = "1.14.0";
  # AppImage asset name published by build-installers.yml (Tauri deb/appimage naming).
  appImageName = "pares-agens_amd64.AppImage";

  src = pkgs.fetchurl {
    url = "https://github.com/plures/pares-agens/releases/download/v${version}/${appImageName}";
    # Placeholder — replace with the real hash via `nix-prefetch-url` (see header).
    # This is intentionally NOT a fake "valid-looking" hash: builds will fail loudly
    # until a real per-release hash is pinned. (C-NOSTUB-001: honest, not faked.)
    sha256 = pkgs.lib.fakeSha256;
  };
in
pkgs.appimageTools.wrapType2 {
  pname = "pares-agens";
  inherit version src;

  extraPkgs = pkgs: with pkgs; [
    webkitgtk_4_1
    gtk3
    libappindicator-gtk3
    librsvg
    openssl
  ];

  extraInstallCommands = ''
    install -Dm444 ${pkgs.appimageTools.extract { inherit version src; name = "pares-agens-${version}"; }}/pares-agens_amd64.desktop \
      $out/share/applications/com.plures.ParesAgens.desktop 2>/dev/null || true
  '';

  meta = with pkgs.lib; {
    description = "Local-first AI agent desktop (Pares Agens)";
    homepage = "https://github.com/plures/pares-agens";
    license = licenses.bsl11 or licenses.unfree;
    platforms = [ "x86_64-linux" ];
    mainProgram = "pares-agens";
  };
}
