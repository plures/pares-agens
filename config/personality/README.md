# Personality Configuration

Default personality files for pares-agens. These are seeded into PluresDB on first startup.

## Files

- `SOUL.md` — Core behavioral rules and identity
- `IDENTITY.md` — Name, platform, model info
- `USER.md` — Owner information
- `AGENTS.md` — Tools, repos, commands, group chat rules
- `HEARTBEAT.md` — Periodic check-in tasks

## How it works

On startup, `seed_from_directory()` reads these files and stores them as PluresDB personality document nodes. They're included in every system prompt via the cerebellum.

To customize: edit these files and restart, OR use `/personality doc <type> set <content>` to modify at runtime (persists in PluresDB, no restart needed).

## Deployment

Copy to `~/.pares-agens/` on the target machine:
```bash
cp config/personality/*.md ~/.pares-agens/
```

Or symlink:
```bash
ln -s $(pwd)/config/personality/*.md ~/.pares-agens/
```
