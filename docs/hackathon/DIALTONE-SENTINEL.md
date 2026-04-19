# Dialtone Sentinel — Microsoft Global Hackathon 2026

**Event:** Microsoft Global Hackathon, Week of September 14, 2026
**Team:** Kayode Bristol (DTMS, SFTCloudInfra/Krypton)
**Category:** AGC/Infrastructure Innovation

## Elevator Pitch (30 seconds)

"Dialtone Sentinel is a cloud-independent AI operations platform that runs
on your existing server CPUs. No GPU. No cloud API calls. It uses Microsoft's
BitNet 1.58-bit models for local inference, a constraint engine for deployment
gates, and a memory system that learns from every deployment outcome. It runs
as an Azure Local extension on the same fleet it manages."

## Problem Statement

DTMS manages dialtone services across Azure Global Cloud — VMs, Ceph clusters,
K8s deployments across connected and sovereign domains. Current challenges:

1. **No AI for ops decisions** — server fleet has CPUs, no GPUs. Cloud AI APIs
   have latency, cost, and compliance issues for production infrastructure.
2. **Manual deployment gates** — AzSecPak windows, Ceph health checks, blast
   radius assessment are human decisions with no institutional memory.
3. **No learning loop** — the same deployment mistake can happen twice because
   outcomes aren't captured as reusable knowledge.
4. **Sovereign compliance complexity** — USME/Fairfax deployments have
   different constraints than corp/pme/ame, tracked manually.

## Solution

An intelligent operations agent that:

### 1. Runs on Existing Hardware (BitNet)
- Microsoft's BitNet 1.58-bit models: 0.2 bytes/param, CPU-native
- 8B model fits in 1.6GB RAM, runs at 5-7 tok/s on server Xeons
- Multiple specialized experts per node (deployment, routing, compliance)
- 7-node cluster = 40+ concurrent experts, ~320B effective parameters
- **Zero GPU. Zero cloud API. Zero additional hardware cost.**

### 2. Declares Logic as Rules, Not Code (.px)
```
constraint no_deploy_during_azsecpak:
  when: deployment.target == usme
  require: deployment.azsecpak_window == false
  severity: error
  message: "Cannot deploy to USME during AzSecPak rollout"

rule safe_deploy_gate:
  when: event.type == "deploy_requested"
  let blast = assess_blast_radius(event.changes)
  let ceph_ok = check_ceph_health(event.target_cluster)
  then:
    - if blast > 3: action: require_approval
    - if NOT ceph_ok: action: block_deploy
    - action: proceed_with_monitoring
```

### 3. Remembers Everything (PluresDB)
- Every deployment outcome stored as a searchable fact
- "Last time we deployed to USME on a Friday, Ceph degraded" → recalled
- Semantic search: relevant history surfaces before every decision
- P2P sync: all nodes share the same memory

### 4. Audits Every Decision (Chronos)
- Full trace: who requested → what was checked → why approved/denied
- Decision ledger with evidence links
- Compliance-ready audit trail

## Architecture

```
DTMS Server Fleet (existing Xeon nodes)
├─ Node 1: BitNet experts (deploy, monitor) + pares-agens + PluresDB
├─ Node 2: BitNet experts (routing, compliance) + pares-agens + PluresDB
├─ Node N: BitNet experts (capacity, triage) + pares-agens + PluresDB
└─ All connected via Hyperswarm P2P (encrypted)

Orchestration: pares-rector (goal-based, self-healing)
Rules: .px files (declarative, auditable, modifiable)
Memory: PluresDB (graph + vector search + P2P sync)
Audit: Chronos (full decision trace)
Interface: Teams bot + ADO dashboard
```

## Demo Flow (5 minutes)

1. **Show the .px rules** — human-readable deployment constraints
2. **Trigger a deployment** — "Deploy monitoring stack v2.3 to DB3 test cluster"
3. **Watch the agent think** — checks constraints, queries memory, assesses blast radius
4. **See it block** — "AzSecPak window active on USME — deployment blocked"
5. **Change the target** — "Deploy to corp instead"
6. **Watch it approve** — constraints pass, memory shows last corp deploy succeeded
7. **Show the audit trail** — every decision with evidence, Chronos trace
8. **The kicker** — "This is running on the same CPU that hosts your VMs. No GPU. No cloud."

## IC4 Alignment

| Pillar | Demonstration |
|---|---|
| **Security First** | Sovereign compliance gates, AzSecPak coordination |
| **Sovereignty & Trust** | MSC parity enforcement, data never leaves fleet |
| **Resilience** | Blast radius analysis, automated rollback, memory-informed decisions |
| **Engineering Excellence** | Zero-touch deployment pipeline, .px declarative rules |
| **AI-Native** | BitNet CPU inference, 3-consciousness routing, learning from outcomes |

## Technical Stack

| Component | Role | Status |
|---|---|---|
| pares-agens | Agent runtime (Rust, 24 crates) | v0.17.0 released |
| PluresDB | Memory + fact storage | Production |
| Praxis (.px) | Rule engine | Grammar + parser + compiler built |
| BitNet | CPU inference | Architecture designed, crates/inference planned |
| pares-rector | Orchestration | Goal spec + self-healing designed |
| Chronos | Audit trail | Wired into agent |
| Hyperswarm | P2P sync | Integrated in PluresDB |
| design-dojo | UI components | v0.10.17 |

## Timeline to Hackathon

| Month | Milestone |
|---|---|
| **May** | .px runtime engine, pares-agens v1.0 on praxisbot + 2nd server |
| **June** | BitNet CPU inference (crates/inference + bitnet.cpp FFI) |
| **July** | pares-rector orchestration, multi-node distributed inference |
| **August** | DTMS .px rules, Teams bot, ADO integration, demo polish |
| **Sep 14** | Hackathon week — demonstrate on test cluster |

## What Microsoft Gets vs What Stays Personal

| Deliverable | Owner |
|---|---|
| .px deployment rules for DTMS/AGC | Microsoft (domain knowledge) |
| Dialtone Sentinel configuration | Microsoft (hackathon project) |
| Platform (compiler, runtime, agent, DB, UI) | Plures LLC (BSL-1.1) |

## Positioning vs Squad + Agency

DTMS currently uses **Squad** (multi-agent orchestrator on Copilot CLI) with
**Agency** (MCP wiring for ADO, Teams, Mail). Dialtone Sentinel is
**complementary, not competitive**.

### What Squad Does Well
- Multi-agent role orchestration (PM, Docs, Infra agents)
- VS Code Copilot Chat integration
- Agency MCP servers (WorkIQ for ADO, Teams, Calendar)

### What Squad Can't Do (Sentinel fills these gaps)
| Gap | Why It Matters | Sentinel Solution |
|---|---|---|
| **No persistent memory** | Can't learn from past deployments | PluresDB (vector search + P2P sync) |
| **No constraint engine** | Can't enforce deployment gates | Praxis .px rules |
| **No offline inference** | Requires cloud API | BitNet CPU (runs on existing Xeons) |
| **No decision audit trail** | Can't prove why a decision was made | Chronos trace + decision ledger |
| **No learning loop** | Same mistakes repeat | Facts captured, recalled next time |
| **Stateless** | Every session starts fresh | PluresDB persists across restarts |

### Integration Strategy
Squad stays the orchestrator. Sentinel adds:
- `sentinel-memory` MCP server (PluresDB facts accessible to Squad agents)
- `sentinel-gate` MCP server (Praxis constraint checks before deployment)
- `sentinel-inference` MCP server (BitNet local model for offline decisions)

Squad agents call Sentinel via MCP. Best of both worlds.
