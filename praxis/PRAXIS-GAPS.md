# Praxis Gaps

Tracked gaps where the current implementation is honestly incomplete. Each gap
must name the missing behavior, why it is deferred, and the enforcement/closure
it needs. A gap is not a stub: the code around it reports the incompleteness
truthfully rather than fabricating a result.

---

## GAP-0001 — `pluresLM-store/` corpus not yet imported by the OpenClaw migrator

**Status:** open
**Filed:** 2026-07-06 (Epic-B step B1)
**Area:** `crates/cli` — `openclaw.rs` / `migrate.rs` (OpenClaw → pares-agens migration)

### What is missing
The migrator reads and imports the `chunks` table of OpenClaw's
`memory/main.sqlite` (the primary memory corpus) into pares-agens
`MemoryEntry` records. OpenClaw also maintains a **separate** `pluresLM-store/`
directory (its own PluresLM store: `db`, `blobs/`, `snap.*`, `conf`). That
second store is **detected and reported** (presence + total size) but its
entries are **not yet imported**.

### Why deferred (honest, not a stub)
- The `pluresLM-store/` on-disk format is a distinct store (not the SQLite
  `chunks` schema) and needs its own reader; reverse-engineering/validating that
  format is a separate unit of work from the SQLite import that closes the bulk
  of B1's memory-retention goal.
- The migration report **never reports memory as empty/zero when a corpus
  exists**: it states `main.sqlite: N chunks imported (size)` and
  `pluresLM-store/: present (size), import pending (see praxis-gap)`. The
  incompleteness is visible in every dry-run, so nothing is silently dropped and
  no count is fabricated.

### Closure criteria
1. Implement a reader for the `pluresLM-store/` format (parse `db` + `blobs/`)
   producing `MemoryEntry` records with provenance tags, mirroring the SQLite
   chunk reader in `openclaw.rs::read_chunks`.
2. Import those entries in `migrate.rs::run` alongside sqlite chunks + legacy
   memories, and count them explicitly in `MigrationReport`.
3. Real unit tests against a small fixture `pluresLM-store/` built in a temp dir.
4. Update the migration report line so `pluresLM-store/` shows an imported count
   (not "import pending").

### Enforcement (per foundational-engineering.px `adr_requires_enforcement`)
When closed, add a test asserting that a populated `pluresLM-store/` yields a
non-zero `MigrationReport` contribution, so a future regression that silently
drops it fails CI — the same class of guard that the SQLite `read_chunks`
missing-table test provides today (a missing `chunks` table is a hard error, not
a silent zero).
