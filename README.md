# Pares Agens

> "one who acts" — local-first AI

AI agent framework for the Pares mesh. Local-first intelligence with PluresDB memory, pluresLM for long-term recall, and cross-platform capability nodes. Agents run on your hardware, not in the cloud.

## Getting Started

### Quick Installation

```bash
# Via Nix profile
nix profile install github:plures/pares-agens

# Via universal installer
curl -sSL install-pares.plures.io | sh

# NixOS configuration
services.pares-agens.enable = true;
```

### First Setup

```bash
# Initialize agent with local memory
pares agens init --name="my-assistant"

# Connect to capability nodes (Windows/macOS desktop automation)
pares agens discover-nodes

# Start agent conversation
pares agens chat
```

### Self-Sovereign AI Setup (Recommended)

For maximum privacy and capability, run local inference:

```bash
# Configure local Qwen3 235B-A22B model cluster
pares agens setup-local-inference \
    --nodes=3 \
    --memory=48gb-each \
    --model="qwen3-235b-a22b"

# Verify local inference is working
pares agens test-inference
```

## Architecture

Pares Agens separates the **agent brain** (reasoning, memory, conversation) from **platform capabilities** (screen, GUI, apps, sensors), enabling true cross-platform operation:

```
    ┌─────────────────────────────────────────────────────────┐
    │              Pares Agens Core (runs anywhere)            │
    │   - Reasoning, planning, memory, conversation             │
    │   - Platform-agnostic (Linux, Mac, container, cloud)     │
    │   - pluresLM unified memory (conversations + actions)     │
    └──────────────────────┬──────────────────────────────────┘
                           │ Pares Protocol (WebSocket/Hyperswarm)
              ┌────────────┼────────────┬────────────┐
              ▼            ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
        │ Windows  │ │  macOS   │ │  Mobile  │ │ Browser  │
        │ Desktop  │ │ Desktop  │ │   Node   │ │   Node   │
        │   Node   │ │   Node   │ │(iOS/And) │ │(existing)│
        └──────────┘ └──────────┘ └──────────┘ └──────────┘
```

### Core Components

- **pluresLM Memory**: Unified memory system storing conversations, code patterns, AND desktop interactions
- **Capability Nodes**: Lightweight binaries providing platform-specific capabilities (GUI automation, sensors, etc.)
- **Local Inference**: Self-sovereign AI with Qwen3 235B-A22B running on your hardware
- **Cross-Platform Protocol**: Unified interface to any OS or device

### Key Innovations

1. **Unified Memory**: First AI agent with comprehensive memory spanning conversations, code, and real-world desktop interactions
2. **Executable Presentations**: Live demos that run actual software, adapt to real results, not just recorded videos
3. **True Local-First**: Runs completely offline with local inference - no cloud dependencies
4. **Cross-Platform by Design**: One agent brain, many hands (nodes on each platform)

## Key Features

### Self-Sovereign Intelligence

```bash
# All AI processing happens on your hardware
# 3x Mac Mini M4 Pro (48GB each) = 144GB total memory
# Runs Qwen3 235B-A22B locally at ~30 tokens/sec
pares agens status
# ✅ Local inference: Qwen3 235B-A22B
# ✅ Memory: pluresLM (127,432 memories)  
# ✅ Capabilities: 3 nodes (Windows, macOS, Browser)
# ✅ Privacy: No cloud dependencies, all local
```

### Comprehensive Memory

pluresLM remembers everything:
- **Conversations**: What you discussed and when
- **Code patterns**: Solutions, errors fixes, architecture decisions  
- **Desktop actions**: What you clicked, built, automated
- **Cross-modal queries**: "Last time we used Calculator's programmer mode, what was the workflow?"

### Cross-Platform Automation

```bash
# Agent can control any platform through capability nodes
pares agens run "Take a screenshot of my VS Code window, then open Calculator and compute 2+2"

# Works whether agent runs in WSL, macOS, Docker, or cloud
# Capability nodes provide the platform-specific "hands"
```

### Executable Presentations

Create live demos that actually run software:

```bash
# Create a presentation that demonstrates pluresLM
pares agens create-demo "pluresLM-introduction" \
    --scenes="install,configure,demo-search,show-memory" \
    --adaptive-narration

# Present it live - runs actual commands, adapts to real results
pares agens present "pluresLM-introduction"
```

### Remote Device Security Management

When a device is lost or stolen, pares-agens can remotely lock it down — going far beyond standard "Find My Device" because a persistent Pares Manus node with root-level access runs on each enrolled device:

```bash
# From any device or web frontend
pares agens run "My phone was stolen. Lock it down."

# pares-agens will:
# 1. Locate the phone node via PluresDB mesh
# 2. Send commands via Hyperswarm (Noise-encrypted P2P)
# 3. Phone's Pares Manus node executes:
#    - Force lock + change PIN
#    - Display "This device has been reported stolen" on lock screen
#    - Encrypt storage (if not already enabled)
#    - Begin GPS tracking
#    - Capture front-camera photo (if Tier 4 was opted in during setup)
# 4. Report back with location + photo + immutable audit record
```

Four security tiers scale from passive location to active investigation (Tier 4 requires explicit opt-in during device setup):

| Tier | Examples |
|---|---|
| **Tier 1 — Locate & Alert** | GPS tracking, play loud sound, lock screen message |
| **Tier 2 — Secure** | Force lock, change password, disable USB/Bluetooth |
| **Tier 3 — Protect Data** | Trigger disk encryption, wipe local DB, revoke API keys |
| **Tier 4 — Investigate** (opt-in) | Camera capture, mic recording, screen capture, geofence alerts |

### Privacy by Design

- **Local inference**: All AI reasoning happens on your hardware
- **Encrypted transport**: All agent-node communication via Noise protocol
- **No telemetry**: Zero data leaves your network unless you explicitly share
- **Audit transparency**: All interactions stored in local pluresLM for review

## Status

🚧 **Pre-alpha** — Architecture and design phase.

### Milestones

- [ ] **Phase 1: Core Agent** (Q2 2026)
  - [ ] pluresLM memory integration
  - [ ] Local inference setup (Qwen3 235B-A22B)
  - [ ] Cross-platform protocol implementation  
  - [ ] Windows/macOS capability nodes
  - [ ] Basic conversation and automation

- [ ] **Phase 2: Advanced Memory** (Q3 2026) 
  - [ ] Desktop interaction memory categories
  - [ ] Cross-modal memory queries
  - [ ] Executable presentation engine
  - [ ] Learn-by-demonstration workflows
  - [ ] Mobile capability nodes (iOS/Android)

- [ ] **Phase 3: Mesh Integration** (Q4 2026)
  - [ ] PluresDB memory sync across devices
  - [ ] Team knowledge sharing
  - [ ] Pares Rector orchestration integration
  - [ ] Enterprise deployment tools
  - [ ] Remote device security management (Find My Device++)
    - [ ] Tier 1: Locate & Alert (GPS, play sound, lock screen message)
    - [ ] Tier 2: Secure (force lock, password change, disable USB/Bluetooth)
    - [ ] Tier 3: Protect Data (disk encryption, wipe local DB, revoke API keys)
    - [ ] Tier 4: Investigate — opt-in only (camera, mic, screen capture, geofence)

## Documentation

- **[Cross-Platform Agent Architecture](docs/CROSS-PLATFORM-AGENT-ARCHITECTURE.md)** — How capability nodes enable cross-platform operation
- **[pluresLM Desktop Memory Integration](docs/PLURESLM-DESKTOP-MEMORY.md)** — Unified memory system spanning conversations and actions
- **[Pares Nubis Cloud Replica](docs/PARES-NUBIS-CLOUD-REPLICA.md)** — Managed always-on cloud peer for backup sync and web access
- **[Remote Device Security Management](docs/REMOTE-DEVICE-SECURITY.md)** — Four-tier lost/stolen device security via Pares Manus nodes
- **[Development Guide](https://github.com/plures/development-guide)** — Cross-cutting concerns and standards

## Part of Pares

pares-agens is part of the [Pares](https://github.com/plures/pares) mesh ecosystem:

| Product | Latin | Role |
|---|---|---|
| **Pares Arca** | "strongbox" | Distributed cache (free tier) |
| **Pares Agens** | "one who acts" | AI agent framework |
| **Pares Manus** | "hands" | Capability nodes (Windows/macOS/mobile) |
| **Pares Nubis** | "cloud" | Managed cloud replica + web frontend |
| **Pares Rector** | "one who steers" | Goal-based orchestrator |
| **Arcae Nexus** | "strongboxes + connection" | Decentralized object registry |
| **Pares Protocol** | — | Wire protocol + command channel |
| **Pares Nix** | — | NixOS config generation |

All components share [PluresDB](https://github.com/plures/pluresdb) as the data plane and [Hyperswarm](https://github.com/plures/hyperswarm) for P2P connectivity.

### Integration Benefits

- **Pares Arca**: Agent learns from your build patterns, optimizes caching strategies
- **Pares Rector**: Agent-driven infrastructure orchestration using learned admin patterns  
- **Pares Manus**: Enhanced capability nodes for specialized hardware control
- **Arcae Nexus**: Share successful automation workflows as reusable packages

## Local Inference Requirements

### Recommended Hardware

**Budget Setup**: Single machine with 64GB+ RAM
- **Model**: Qwen3 70B (quantized)
- **Performance**: ~15 tokens/sec
- **Cost**: ~$3,000

**Optimal Setup**: 3x Mac Mini M4 Pro (48GB each)  
- **Model**: Qwen3 235B-A22B (full precision)
- **Performance**: ~30 tokens/sec reasoning, ~100 tokens/sec simple queries
- **Cost**: ~$6,000 total
- **Benefits**: True self-sovereignty, maximum privacy, enterprise-grade capability

### Why Local Inference Matters

- **Privacy**: Your conversations and automations never leave your network
- **Reliability**: Works completely offline, no internet dependencies
- **Performance**: Lower latency than cloud APIs for many queries  
- **Cost**: No per-token charges, unlimited usage
- **Sovereignty**: Complete control over your AI capabilities

## Contributing

Pares Agens is open source under AGPL-3.0. See the [development guide](https://github.com/plures/development-guide) for contribution guidelines, coding standards, and architecture decisions.

## License

AGPL-3.0