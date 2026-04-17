# NixOS Deployment Guide

Pares Agens provides a NixOS flake with a ready-to-use NixOS module.

## Quick Start

### 1. Add the flake input

```nix
# flake.nix
{
  inputs = {
    pares-agens.url = "git+ssh://git@github.com/plures/pares-agens";
    # Or for public access (when available):
    # pares-agens.url = "github:plures/pares-agens";
  };
}
```

### 2. Import the NixOS module

```nix
# In your host configuration or flake-parts extraModules:
{
  extraModules = [
    inputs.pares-agens.nixosModules.default
  ];
}
```

### 3. Handle the BSL-1.1 License

Pares Agens is licensed under [BSL-1.1](https://mariadb.com/bsl11/) (Business Source License 1.1). Nix treats this as an **unfree** license. You must explicitly allow it.

**Option A — Allow all unfree packages (simplest):**

If you're using **flake-parts**, set `nixpkgsConfig` in your host definition:

```nix
# hosts/myhost/host.nix
{
  nixpkgsConfig = {
    allowUnfree = true;
  };
}
```

If you're using a **standalone flake** (no flake-parts), set it where you import nixpkgs:

```nix
pkgs = import nixpkgs {
  inherit system;
  config.allowUnfree = true;
};
```

**Option B — Allow only pares-agens:**

```nix
# flake-parts host config:
nixpkgsConfig = {
  allowUnfreePredicate = pkg:
    builtins.elem (nixpkgs.lib.getName pkg) [ "pares-agens" ];
};
```

> **⚠️ Do NOT use `nixpkgs.config.allowUnfree = true;` in `configuration.nix`** when your flake creates the nixpkgs instance externally (e.g. flake-parts). This will cause:
> ```
> error: Your system configures nixpkgs with an externally created instance.
> nixpkgs.config options should be passed when creating the instance instead.
> ```
> Use `nixpkgsConfig` in your host definition instead.

### 4. Enable the service

```nix
# configuration.nix or a dedicated module
{
  services.pares-agens = {
    enable = true;
    copilot = true;                           # Use GitHub Copilot for LLM
    model = "gpt-4.1";                        # Conscious model
    deepModel = "claude-opus-4.6";            # Deep escalation model
    telegramTokenFile = "/run/secrets/telegram-token";
    braveApiKeyFile = "/run/secrets/brave-key";
  };
}
```

### 5. Build and switch

```bash
sudo nix flake update pares-agens
sudo nixos-rebuild switch --flake .#myhost
```

### 6. Authorize Copilot (one-time)

On first start, the service will print a device flow code:

```bash
journalctl -u pares-agens -f
# Look for: "visit https://github.com/login/device and enter code XXXX-XXXX"
```

Visit the URL, enter the code, and the OAuth token is cached permanently.

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable` | bool | `false` | Enable the service |
| `copilot` | bool | `true` | Use GitHub Copilot OAuth for LLM access |
| `model` | string | `"gpt-4.1"` | Conscious model (80% of requests) |
| `deepModel` | string | `"claude-opus-4.6"` | Deep model for low-confidence escalation |
| `user` | string | `"pares-agens"` | Service user |
| `group` | string | `"pares-agens"` | Service group |
| `createUser` | bool | `true` | Create the service user (set `false` for existing users) |
| `dataDir` | path | `/var/lib/pares-agens` | PluresDB storage + config directory |
| `telegramTokenFile` | path | `null` | Path to file containing Telegram bot token |
| `braveApiKeyFile` | path | `null` | Path to file containing Brave Search API key |
| `systemPromptFile` | path | `null` | Path to custom system prompt |
| `extraFlags` | list | `[]` | Additional CLI flags |

## Running as an Existing User

To inherit an existing user's tools (gh, git, SSH keys, sudo):

```nix
{
  services.pares-agens = {
    enable = true;
    user = "myuser";
    group = "users";
    createUser = false;
    dataDir = "/home/myuser/.pares-agens";
  };
}
```

## Private Repo Access

If fetching from the private GitHub repo, Nix needs authentication:

**SSH (recommended):** Use `git+ssh://` URL in your flake input (requires SSH key with repo access).

**HTTPS with token:** Add to your Nix config:
```
# ~/.config/nix/nix.conf or via nix.extraOptions
access-tokens = github.com=ghp_your_token_here
```

## Updating

```bash
sudo nix flake update pares-agens
sudo nixos-rebuild switch --flake .#myhost
```

The service restarts automatically after rebuild.
