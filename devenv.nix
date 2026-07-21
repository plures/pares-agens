{ pkgs, lib, ... }:

{
  languages.rust = {
    enable = true;
    channel = "stable";
  };

  languages.javascript = {
    enable = true;
    pnpm.enable = true;
  };

  packages = with pkgs; [
    nodejs
    cargo-tauri
    pkg-config
    wrapGAppsHook4

    webkitgtk_4_1
    libsoup_3
    gtk3
    glib
    gobject-introspection
    librsvg
    openssl
    zlib
  ];

  env = {
    WEBKIT_DISABLE_COMPOSITING_MODE = "1";
  };

  enterShell = ''
    echo "rustc: $(rustc --version)"
    echo "cargo: $(cargo --version)"
    echo "node: $(node --version)"
    echo "pnpm: $(pnpm --version)"
    echo "tauri: $(cargo tauri --version)"
  '';
}
