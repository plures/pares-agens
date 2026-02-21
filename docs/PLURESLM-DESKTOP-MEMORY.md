# pluresLM Desktop Memory Integration

**Component**: Pares Agens Memory System  
**Source**: development-guide/design/PLURESLM-DESKTOP-MEMORY-AND-PRAXIS-SCRIPTS.md
**Status**: Design Phase
**Last Updated**: 2026-02-17

This document defines how Pares Agens integrates pluresLM as the unified memory substrate for conversations, code, AND desktop interactions — making the agent's memory system comprehensive and queryable across all modalities.

## The Big Idea

Pares Agens uses **pluresLM as the single memory substrate for everything the agent perceives and does** — conversations, code patterns, *and* desktop interactions. The capability nodes aren't separate tools; they're **pluresLM's eyes and hands** extending the agent's sensorium.

This enables unprecedented capabilities:
- **Cross-modal memory**: "Last time we used Calculator's programmer mode, what was the workflow?"
- **Executable presentations**: Live demos that run on the viewer's machine, adapting to actual results
- **Learn-by-demonstration**: Agent learns workflows by watching user interactions

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                 Pares Agens Core                        │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │             pluresLM Memory                     │   │
│  │   ┌─────────────┐ ┌─────────────┐ ┌──────────┐ │   │
│  │   │Conversation │ │  Code       │ │ Desktop  │ │   │
│  │   │  Memory     │ │  Patterns   │ │ Actions  │ │   │
│  │   │             │ │             │ │          │ │   │
│  │   └─────────────┘ └─────────────┘ └──────────┘ │   │
│  │          ▲               ▲              ▲        │   │
│  └──────────┼───────────────┼──────────────┼────────┘   │
│             │               │              │            │
│    ┌────────▼───────────────▼──────────────▼──────┐     │
│    │        Unified Vector Search Space          │     │
│    │  - Semantic search across all modalities     │     │
│    │  - Temporal causality tracking               │     │
│    │  - Cross-reference conversations ↔ actions   │     │
│    └───────────────────────────────────────────────┘     │
│                                                         │
└─────────────────────┬───────────────────────────────────┘
                      │ Pares Protocol
                      ▼
            ┌─────────────────────┐
            │  Capability Nodes   │
            │ (Windows/macOS/etc) │
            └─────────────────────┘
```

## Desktop Memory Categories

pluresLM gains new memory categories for desktop interactions:

| Category | What It Captures | Example |
|----------|-----------------|---------|
| `ui-interaction` | Click/type/navigate events with before/after state | "Clicked '=' in Calculator. Display changed from '2+2' to '4'" |
| `app-state` | Application window snapshots (title, size, position, key UI values) | "VS Code had `memory-db.ts` open, cursor at line 47, 3 unsaved files" |
| `screen-capture` | Tagged screenshots with semantic region annotations | "Screenshot of Terminal showing `cargo build` output with 2 warnings" |
| `automation-trace` | Full trace of a multi-step automated sequence | "Opened Settings → Network → Wi-Fi → Connected to 'HomeNet' (4 steps, 2.3s)" |
| `build-result` | Build/compile/test outcomes with environment context | "`cargo build --release` succeeded in 34s. 171 crates. Binary: 2.0MB" |
| `demo-checkpoint` | Named state during an executable presentation | "Demo 'pluresLM-intro' reached checkpoint 'build-complete' — all green" |

These coexist with existing categories (`conversation`, `code-pattern`, `error-fix`, `preference`, `decision`) in the same vector space.

## Memory Schema Extension

### Hierarchical Tagging

Every desktop memory gets structured tags:

```
# Hierarchical app tags
app:<name>                    → app:calculator, app:vscode, app:terminal
window:<title-slug>           → window:calculator, window:memory-db-ts-vscode
uia:<automationId>            → uia:equalButton, uia:DisplayControl

# Action tags
action:click                  → what was done
action:type
action:launch
action:build
action:navigate

# Result tags
result:success                → outcome
result:error
result:changed                → state changed vs. didn't
result:unchanged

# Context tags
context:demo:<demo-id>        → part of an executable presentation
context:task:<task-id>         → part of a larger automation task
context:session:<session-key>  → which agent session
```

### Enhanced Memory Metadata

The `MemoryEntry` interface gains optional `metadata` for desktop interactions:

```typescript
interface DesktopMemoryMetadata {
  // Application context
  app?: string;                     // "Calculator"
  windowTitle?: string;             // "Calculator"
  windowHandle?: number;            // Win32 HWND

  // UI element details
  element?: {
    name?: string;                  // "Equals"
    controlType?: string;           // "Button"
    automationId?: string;          // "equalButton"
    className?: string;             // "Windows.UI.Core.CoreWindow"
    path?: string;                  // "Window/Group/Button[=]"
    boundingRect?: { x: number; y: number; w: number; h: number };
  };

  // Action taken
  action?: "click" | "type" | "launch" | "focus" | "close" | "navigate" | "build" | "resize" | "scroll" | "key";
  actionParams?: Record<string, unknown>;  // e.g. { text: "hello" } for type

  // State before and after
  beforeState?: {
    summary: string;                // "Display showing '2+2'"
    screenshotRef?: string;         // memory ID of screenshot memory
    uiTreeRef?: string;             // memory ID of UI tree snapshot
    values?: Record<string, string>; // key UI element values
  };
  afterState?: {
    summary: string;                // "Display showing '4'"
    screenshotRef?: string;
    uiTreeRef?: string;
    values?: Record<string, string>;
  };

  // Timing and causality
  durationMs?: number;              // how long the action took
  timestamp?: string;               // ISO 8601
  triggeredBy?: string;             // memory ID of the action/event that caused this
  triggeredActions?: string[];       // memory IDs of follow-up actions

  // Demo context
  demoId?: string;                  // if this happened during an executable presentation
  checkpointId?: string;            // named checkpoint in a demo
  sceneId?: string;                 // which scene of the demo
}
```

## Storage Implementation

### Database Schema

```sql
-- Existing memories table gains metadata column
ALTER TABLE memories ADD COLUMN metadata TEXT DEFAULT '{}';
-- JSON-encoded DesktopMemoryMetadata

-- Graph edges for temporal causality
CREATE TABLE memory_edges (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    relation TEXT NOT NULL,  -- "caused", "preceded", "part-of", "referenced"
    weight REAL DEFAULT 1.0,
    metadata TEXT DEFAULT '{}',
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (from_id, to_id, relation)
);
```

### Cross-Modal Queries

The unified memory enables queries that span modalities:

```rust
// Find all calculator usage in the last week
let memories = pluresLM.search_memories(
    "calculator math computation",
    &SearchOptions {
        categories: Some(vec!["ui-interaction", "conversation", "screen-capture"]),
        tags: Some(vec!["app:calculator"]),
        after: Some(Utc::now() - Duration::days(7)),
        ..Default::default()
    }
).await?;

// Get the conversation context for a specific UI action
let click_memory = pluresLM.get_memory("click-equals-button-123").await?;
let context = pluresLM.get_causal_chain(
    &click_memory.id,
    ChainDirection::Backward,
    5  // Look back 5 steps
).await?;
```

## Self-Sovereign AI Integration

Pares Agens runs local inference for maximum privacy and capability:

### Hardware Requirements

**Recommended Setup**: 3x Mac Mini M4 Pro (48GB RAM each)
- **Total**: 144GB unified memory, 36 GPU cores
- **Model**: Qwen3 235B-A22B (locally hosted)
- **Performance**: ~30 tokens/sec for reasoning, ~100 tokens/sec for simple queries

### Local Inference Stack

```
┌─────────────────────────────────────────────────────┐
│                Mac Mini Cluster                      │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐           │
│  │  Node 1  │ │  Node 2  │ │  Node 3  │           │
│  │  (Main)  │ │(Parallel)│ │(Parallel)│           │
│  │ 48GB RAM │ │ 48GB RAM │ │ 48GB RAM │           │
│  │ Qwen3    │ │ Qwen3    │ │ Qwen3    │           │
│  │ Primary  │ │ Shard A  │ │ Shard B  │           │
│  └──────────┘ └──────────┘ └──────────┘           │
└─────────────────┬───────────────────────────────────┘
                  │ Model serving API
┌─────────────────▼───────────────────────────────────┐
│              Pares Agens Core                       │
│  ┌─────────────────────────────────────────────┐   │
│  │            pluresLM Memory                  │   │
│  │  - Vector embeddings (local Sentence-BERT)  │   │
│  │  - Semantic search across all modalities    │   │
│  │  - Conversation + desktop action correlation │   │
│  └─────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

### Privacy Benefits

- **No cloud dependencies**: All AI inference happens locally
- **No data leakage**: Desktop interactions never leave your network
- **Network-optional**: Agent works completely offline once installed
- **Audit transparency**: All model weights and inference logic are local

## Executable Presentations

One of the most innovative features enabled by comprehensive desktop memory:

### Concept

Instead of recording videos or creating static slides, create **executable presentations** — live demos that:
1. Run actual software on the viewer's machine
2. Adapt to real results (what if the build fails?)
3. Provide narration that matches what actually happened
4. Can be paused, rewound, and explored interactively

### Implementation

```typescript
// Define a presentation as a sequence of scenes
interface ExecutablePresentation {
    id: string;
    title: string;
    description: string;
    scenes: PresentationScene[];
    checkpoints: PresentationCheckpoint[];
}

interface PresentationScene {
    id: string;
    title: string;
    narration: string;              // What to say during this scene
    actions: PresentationAction[];  // What to do
    adaptations: SceneAdaptation[]; // How to handle different outcomes
}

interface PresentationAction {
    type: "ui-action" | "shell-command" | "wait" | "checkpoint";
    target?: string;                // UI selector or shell command
    params?: Record<string, unknown>;
    timeout?: number;
    required?: boolean;             // Fail presentation if this fails
}

interface SceneAdaptation {
    condition: string;              // pluresLM query to detect scenario
    narration: string;             // Alternative narration
    actions?: PresentationAction[]; // Alternative actions
}
```

### Example: pluresLM Introduction Demo

```json
{
    "id": "pluresLM-intro",
    "title": "pluresLM: AI Memory That Actually Remembers",
    "description": "Live demo showing pluresLM learning from code and conversations",
    "scenes": [
        {
            "id": "open-terminal",
            "title": "Opening Terminal",
            "narration": "Let's start by opening a terminal and navigating to our project directory.",
            "actions": [
                { "type": "ui-action", "target": "app.launch", "params": { "app": "terminal" } },
                { "type": "shell-command", "target": "cd ~/dev/pluresLM-demo", "timeout": 2000 },
                { "type": "checkpoint", "params": { "id": "terminal-ready" } }
            ],
            "adaptations": [
                {
                    "condition": "error:directory-not-found",
                    "narration": "Looks like the demo directory doesn't exist yet. Let's create it.",
                    "actions": [
                        { "type": "shell-command", "target": "mkdir -p ~/dev/pluresLM-demo" },
                        { "type": "shell-command", "target": "cd ~/dev/pluresLM-demo" }
                    ]
                }
            ]
        },
        {
            "id": "install-pluresLM", 
            "title": "Installing pluresLM",
            "narration": "Now we'll install pluresLM using cargo. This might take a moment.",
            "actions": [
                { "type": "shell-command", "target": "cargo install pluresLM", "timeout": 30000 },
                { "type": "checkpoint", "params": { "id": "installation-complete" } }
            ],
            "adaptations": [
                {
                    "condition": "result:success AND duration:<10s",
                    "narration": "Great! pluresLM installed quickly — looks like it was already cached."
                },
                {
                    "condition": "result:error AND error:network",
                    "narration": "Hmm, network issues. Let's try installing from a local copy instead.",
                    "actions": [
                        { "type": "shell-command", "target": "cargo install --path ./local-pluresLM" }
                    ]
                }
            ]
        }
    ]
}
```

### Demo Engine

The presentation engine uses pluresLM's memory system to:

1. **Track state**: Each action's results are stored as memories
2. **Adapt narration**: Query memories to understand what actually happened
3. **Handle failures**: Use adaptations to gracefully handle unexpected outcomes
4. **Enable interaction**: Viewer can pause and ask questions about what they're seeing

```rust
impl PresentationEngine {
    async fn run_scene(&mut self, scene: &PresentationScene) -> Result<SceneResult> {
        // Execute each action, storing results in pluresLM
        for action in &scene.actions {
            let result = self.execute_action(action).await?;
            
            // Store in memory with demo context
            self.pluresLM.store_memory(MemoryEntry {
                content: format!("Demo action: {} completed", action.type),
                category: "demo-checkpoint".to_string(),
                tags: vec![
                    format!("demo:{}", self.demo_id),
                    format!("scene:{}", scene.id),
                    format!("action:{}", action.type),
                ],
                metadata: Some(DesktopMemoryMetadata {
                    demo_id: Some(self.demo_id.clone()),
                    scene_id: Some(scene.id.clone()),
                    action: Some(action.type.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            }).await?;
        }
        
        // Determine which adaptation (if any) to use
        let adaptation = self.find_matching_adaptation(&scene.adaptations).await?;
        
        Ok(SceneResult {
            success: true,
            adaptation_used: adaptation,
            memories_created: scene.actions.len(),
        })
    }
    
    async fn find_matching_adaptation(&self, adaptations: &[SceneAdaptation]) -> Option<SceneAdaptation> {
        for adaptation in adaptations {
            // Query pluresLM to see if this adaptation's condition matches
            let matches = self.pluresLM.search_memories(
                &adaptation.condition,
                &SearchOptions {
                    categories: Some(vec!["demo-checkpoint"]),
                    tags: Some(vec![format!("demo:{}", self.demo_id)]),
                    limit: Some(1),
                    ..Default::default()
                }
            ).await.ok()?.len() > 0;
            
            if matches {
                return Some(adaptation.clone());
            }
        }
        None
    }
}
```

## Integration with Pares Mesh

Pares Agens connects to the broader Pares ecosystem:

### PluresDB Integration
- **Shared memory**: Agent memories sync across devices via PluresDB CRDT
- **Cross-device learning**: Desktop interactions on laptop inform agent behavior on desktop
- **Team knowledge**: Optional sharing of learned workflows within team topics

### Hyperswarm Connectivity
- **Capability discovery**: Find available nodes (Windows, macOS, mobile) via Hyperswarm
- **Secure transport**: All agent ↔ node communication via Noise protocol
- **Network resilience**: Automatic reconnection and failover

### Future Integration
- **Pares Rector**: Agent-driven orchestration using learned system administration patterns
- **Pares Manus**: Enhanced capability nodes for specialized hardware
- **Arcae Nexus**: Sharing of successful automation workflows as packages

---

*This architecture makes Pares Agens the first AI agent with truly comprehensive memory — spanning conversations, code, and real-world desktop interactions in a unified, queryable system.*