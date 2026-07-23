# Pares Agens on NixOS / Nix

End-user install of the **public pre-built AppImage** from GitHub Releases.
No private repo access, no token, no source build.

## Quick run (flake)

```sh
nix run "github:plures/pares-agens?dir=packaging/nix"
```

## Install into your profile

```sh
nix profile install "github:plures/pares-agens?dir=packaging/nix"
```

## Use in a NixOS config via overlay

```nix
{
  inputs.pares-agens.url = "github:plures/pares-agens?dir=packaging/nix";

  # in your nixpkgs config:
  nixpkgs.overlays = [ inputs.pares-agens.overlays.default ];
  environment.systemPackages = [ pkgs.pares-agens ];
}
```

## Pinning a specific release hash

The derivation fetches the AppImage from
`https://github.com/plures/pares-agens/releases/download/v<version>/pares-agens_amd64.AppImage`.

The committed `default.nix` uses `lib.fakeSha256` as the `src.sha256` placeholder — builds fail
loudly until you pin a real hash (this is intentional; we do not ship a fake-but-valid hash):

```sh
nix-prefetch-url \
  https://github.com/plures/pares-agens/releases/download/v1.14.0/pares-agens_amd64.AppImage
```

Paste the printed hash into `src.sha256` in `default.nix` and bump `version` to match the tag.

> The AppImage asset only appears on a release once `build-installers.yml` attaches it.
> As of writing, recent releases have no assets — see `memory/tasks/dist-track.md` gap #1.
