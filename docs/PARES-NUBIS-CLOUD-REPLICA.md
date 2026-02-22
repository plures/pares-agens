# Pares Nubis — Managed Cloud Replica

**Component**: Pares Nubis - Managed Cloud Sync & Web Frontend  
**Source**: development-guide/design/PARES-NUBIS-CLOUD-REPLICA.md  
**Status**: Design Phase  
**Last Updated**: 2026-02-22

This document defines the architecture for Pares Nubis, a managed always-on PluresDB peer running in a cloud container (Azure). It serves as a backup sync peer for phone-only users, a web frontend for browser-based access, and a subscription revenue stream.

## Product Name

**Pares Nubis** — Latin *nubes* → Portuguese *nuvem* = "cloud". Fits the Latin product family.

Alternative: **Pares Custos** ("guardian") — emphasises the backup/protection role.

## Problem Statement

Pares is a local-first, peer-to-peer system. This creates a fundamental availability problem: data is only accessible when at least one device is online. For users who rely on a phone as their only device, sync happens only when another device (desktop, laptop) is also connected.

Pares Nubis solves this by providing a managed always-on peer in the cloud:

- **Phone-only users**: data is continuously synced even when no other personal devices are online
- **Public computer access**: browser-based access from any machine without installing software
- **Guaranteed durability**: full history replica in geographically redundant cloud storage

## Architecture

```
┌─────────────────────────────────────────────────┐
│              Pares Nubis (Azure)                │
│  ┌─────────────────────────────────────────────┐ │
│  │  Container (per customer)                   │ │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  │ │
│  │  │ PluresDB │  │Hyperswarm│  │ Web UI   │  │ │
│  │  │ (full    │  │ peer     │  │ (Svelte  │  │ │
│  │  │  replica)│  │          │  │  + WASM  │  │ │
│  │  │          │  │          │  │  PluresDB│  │ │
│  │  └──────────┘  └──────────┘  └──────────┘  │ │
│  └─────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘
           │ Hyperswarm P2P sync
    ┌──────┴──────┐
    ▼             ▼
┌────────┐  ┌────────┐
│ Phone  │  │Desktop │
│ (1 GB) │  │(full)  │
└────────┘  └────────┘
```

### Key Properties

- **Always-on**: cloud container runs 24/7, always available as a sync peer
- **Full replica**: stores complete PluresDB history (phone only keeps recent data)
- **Web access**: Svelte frontend with PluresDB running in browser via WASM — same code as desktop/TUI app
- **Secure**: end-to-end encrypted via Hyperswarm Noise protocol; cloud container holds ciphertext only, keys stay on the user's devices
- **Same topic key**: joins the same Hyperswarm mesh as all other devices — appears as just another peer

## Web Frontend

Because PluresDB runs in the browser (WASM), the web frontend is the same Svelte application that runs in Tauri desktop and compiles to ratatui TUI:

```
Same Svelte components (design-dojo)
├── Desktop: Tauri WebView (full GUI)
├── TUI: svelte-ratatui → native terminal
├── Web: Vite build → static site + WASM PluresDB
└── All share PluresDB backend via Hyperswarm
```

The web frontend connects to the cloud container's PluresDB instance. For public computer access, the user authenticates via a time-limited token/passphrase and the browser loads the Svelte app with PluresDB WASM connecting to their cloud peer.

## Phone Storage Management

`pares-manus` mobile keeps only recent data locally (configurable, default ~1 GB):

```
Phone PluresDB:
├── Last 30 days of messages (always local)
├── Last 7 days of embeddings (for search)
├── Conversation tags (lightweight, always synced)
└── Older data: available on-demand from cloud peer or desktop
```

When the user searches for older data, PluresDB fetches from the nearest available peer (cloud replica or desktop, if online). This is transparent to the application layer.

## Pricing

| Tier | Storage | Features | Price |
|---|---|---|---|
| Free | — | No cloud replica (peer-to-peer only) | $0/mo |
| Starter | 5 GB | Cloud backup peer + web access | $9/mo |
| Pro | 50 GB | + priority sync + multi-agent support | $29/mo |
| Enterprise | Custom | + SLA + dedicated instance + SSO | Custom |

## Implementation Phases

### Phase 1: Container + Sync

- [ ] Docker container image with PluresDB + Hyperswarm
- [ ] Container joins user's Hyperswarm mesh via topic key on startup
- [ ] Full data replication with CRDT conflict resolution (PluresDB)
- [ ] Health monitoring and restart policy
- [ ] Container registry in Azure Container Registry (ACR)

### Phase 2: Web Frontend

- [ ] PluresDB WASM build target
- [ ] Svelte web app (design-dojo components, same as desktop/TUI)
- [ ] Token-based authentication for browser sessions
- [ ] Deploy as Azure Static Web App + API proxy to container
- [ ] Session isolation: each browser tab connects to user's cloud peer only

### Phase 3: Phone Storage Optimisation

- [ ] LRU eviction policy for old data on device
- [ ] On-demand fetch API from cloud peer
- [ ] Bandwidth-aware sync (Wi-Fi vs. cellular toggle)
- [ ] Configurable retention window per data category

### Phase 4: Productionise

- [ ] Azure Container Instances deployment automation
- [ ] Per-customer provisioning API (create / suspend / delete container)
- [ ] Billing integration (Stripe or Azure Marketplace)
- [ ] Admin dashboard: usage, health, storage per customer
- [ ] Geo-redundant storage option for Pro/Enterprise tiers

## Container Design

### Single-Container Layout (per customer)

```
┌──────────────────────────────────────────────────────────┐
│  pares-nubis container                                   │
│                                                          │
│  ┌────────────────┐   ┌────────────────┐                │
│  │  pluresd        │   │  hyperswarm-   │                │
│  │  (PluresDB      │◄──│  bridge        │                │
│  │   daemon)       │   │  (joins mesh)  │                │
│  └────────────────┘   └────────────────┘                │
│                                                          │
│  ┌────────────────────────────────────────────────────┐  │
│  │  web-proxy (nginx / Caddy)                        │  │
│  │  - Serves Svelte WASM bundle (static assets)      │  │
│  │  - Proxies /api/* → pluresd HTTP interface        │  │
│  │  - Terminates TLS                                 │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
│  Volumes:                                                │
│  - /data/pluresdb   (Azure Managed Disk or Files share) │
│  - /run/pluresd.sock (internal IPC)                     │
└──────────────────────────────────────────────────────────┘
```

### Environment Variables

| Variable | Description | Example |
|---|---|---|
| `PARES_TOPIC_KEY` | Hyperswarm topic key for user's mesh | `ed25519:<hex>` |
| `PARES_STORAGE_LIMIT_GB` | Maximum allowed disk usage; on exceed, instance must alert and refuse new writes (no automatic data eviction) | `5` |
| `PARES_WEB_TOKEN_SECRET` | HMAC secret for browser session tokens | `<random-256-bit>` |
| `PARES_HEALTH_PORT` | HTTP port for health/readiness probes | `8080` |
| `PARES_DATA_DIR` | PluresDB data directory | `/data/pluresdb` |

### Health Endpoints

```
GET /healthz   → 200 OK  (liveness probe)
GET /readyz    → 200 OK  (readiness probe, once Hyperswarm peer is joined)
GET /metrics   → Prometheus-format metrics (peer count, storage used, sync lag)
```

## Security Model

1. **End-to-end encryption**: Hyperswarm Noise protocol encrypts all sync traffic. The container only stores and forwards ciphertext it cannot read without the user's key.
2. **Key isolation**: User keys never leave their personal devices. The container holds no private key material beyond the Hyperswarm noise keypair (used for transport, not data decryption).
3. **Browser session tokens**: Short-lived HMAC tokens (1 hour TTL) issued after user authenticates with their passphrase. Tokens are tied to a specific container instance.
4. **Container isolation**: One container per customer. No cross-customer data access at the OS or storage layer.
5. **Least privilege**: Container runs as non-root. Hyperswarm bridge and pluresd run as separate processes with minimal capability set.
6. **Audit trail**: All sync events and web sessions logged to PluresDB for user review.

## Azure Deployment

### Recommended Azure Services

| Service | Usage |
|---|---|
| Azure Container Instances (ACI) | Per-customer container runtime |
| Azure Container Registry (ACR) | Private image registry |
| Azure Files / Managed Disk | Persistent PluresDB storage |
| Azure Static Web Apps | Svelte WASM bundle CDN |
| Azure Key Vault | `PARES_WEB_TOKEN_SECRET` and provisioning credentials |
| Azure Monitor | Container health, metrics, alerting |

### Provisioning Flow

```
User subscribes (Stripe webhook)
    │
    ▼
Provisioning API
    ├── Generate PARES_TOPIC_KEY slot (user provides at first login)
    ├── Create ACI container group
    ├── Attach Azure Files share (/data/pluresdb)
    ├── Inject secrets from Key Vault
    └── Return container endpoint URL to user
```

## Related

- [PluresDB](https://github.com/plures/pluresdb) — browser/WASM build target required for Phase 2
- [design-dojo](https://github.com/plures/design-dojo) — Svelte components shared across desktop, TUI, and web surfaces
- [pares-manus](https://github.com/plures/pares-manus) — mobile phone node (Phase 3 phone optimisation)
- [Cross-Platform Agent Architecture](CROSS-PLATFORM-AGENT-ARCHITECTURE.md) — capability node protocol
- [Hyperswarm](https://github.com/plures/hyperswarm) — P2P connectivity and Noise encryption

---

*Pares Nubis makes the local-first Pares mesh genuinely available everywhere — without sacrificing end-to-end encryption or user sovereignty.*
