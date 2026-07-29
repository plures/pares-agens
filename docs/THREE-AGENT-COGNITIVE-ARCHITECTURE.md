# Design: Three-Agent Cognitive Architecture

**Component**: Pares Agens Core  
**Status**: Approved (restated from lost Praxisbot context)  
**Date**: 2026-03-17  
**Author**: Paradox (restated), documented by mswork

## Overview

Three cognitive components with distinct compute profiles, where the **orchestrator is the central orchestrator** — not the standard agent.

```
                         User Prompt
                              │
                              ▼
                    ┌─────────────────┐
                    │   ORCHESTRATOR    │
                    │  (Orchestrator) │
                    │                 │
                    │ • Fast, small   │
                    │ • Multiple      │
                    │   parallel      │
                    │   agents +      │
                    │   procedures    │
                    │ • Routes,       │
                    │   preprocesses, │
                    │   assembles     │
                    └───┬────────┬────┘
                        │        │
              ┌─────────▼──┐  ┌──▼──────────┐
              │ STANDARD   │  │ DEEP_REASONER │
              │             │  │              │
              │ • Medium    │  │ • High       │
              │   thinking  │  │   thinking   │
              │ • Focused,  │  │ • Background │
              │   directed  │  │ • What-if    │
              │   action    │  │   scenarios  │
              │ • Executes  │  │ • Deep       │
              │   specific  │  │   reasoning  │
              │   tasks     │  │ • Exploration│
              └─────────────┘  └──────────────┘
```

## Component Specifications

### Orchestrator (Orchestrator)
**Compute**: Multiple small, fast agents + database procedures  
**Model tier**: Cheap/fast (e.g., gpt-4o-mini, Haiku)  
**Role**: Central router and coordinator

**On user prompt arrival:**
1. Autorecall — retrieve relevant memories via PluresDB procedures
2. Preprocess prompt — classify intent, extract entities, determine complexity
3. Formulate targeted context for standard and deep_reasoner
4. Route work:
   - Direct the **deep_reasoner** to reason about scenarios, explore what-ifs, or do background research
   - Provide the **standard** with specific targeted information, context, and task prompts
5. Spawn research agents as needed (parallel sub-agents for fact-gathering)
6. Create cron jobs for deferred or recurring work
7. Trigger agents of varying capabilities based on task requirements

**Ongoing responsibilities:**
- Track performance of all agents and procedures
- Make preemptive optimizations:
  - Create/modify procedures
  - Adjust cron schedules
  - Customize memory retrieval patterns
  - Tune context budgets
- Formulate the final response to the user based on all available information from standard, deep_reasoner, and its own processing

### Standard (Executor)
**Compute**: Medium thinking (e.g., Sonnet, GPT-4o)  
**Model tier**: Mid-range, balanced speed/capability  
**Role**: Focused, directed task execution

- Receives **specific targeted context** from orchestrator (not raw memories)
- Executes well-defined tasks with clear scope
- Does NOT self-manage memory or context — orchestrator handles that
- Returns results to orchestrator for assembly

### DeepReasoner (Deep Reasoner)
**Compute**: High thinking (e.g., Opus, o1-pro, extended thinking)  
**Model tier**: Expensive, powerful, latency-tolerant  
**Role**: Background deep processing

- Directed by orchestrator via triggers and procedures
- Background preprocessing of complex information
- What-if scenario exploration
- Deep reasoning on ambiguous or multi-faceted problems
- Results stored in PluresDB for orchestrator to retrieve and route

## Flow: User Prompt → Response

```
1. User sends prompt
2. ORCHESTRATOR receives prompt
   a. Runs autorecall procedure (PluresDB) → retrieves compressed context
   b. Classifies prompt (intent, complexity, urgency)
   c. Checks deep_reasoner state — any pre-computed insights available?
   d. Formulates targeted prompts:
      - STANDARD prompt: "Do X with this specific context"
      - DEEP_REASONER triggers: "Explore Y scenario", "Research Z"
3. STANDARD executes directed task → returns result
4. DEEP_REASONER (async) explores scenarios → stores insights in PluresDB
5. ORCHESTRATOR assembles response:
   - Standard result (primary)
   - DeepReasoner insights (if available, enriches response)
   - Its own preprocessing results
6. ORCHESTRATOR delivers response to user
7. ORCHESTRATOR post-processes:
   - Updates performance metrics
   - Adjusts procedures/cron if patterns detected
   - Stores interaction primitives for future recall
```

## Key Differences from Letta Sleep-time

| Aspect | Letta | Plures Three-Agent |
|--------|-------|-------------------|
| Orchestration | Primary agent drives; sleep agent is passive background | Orchestrator orchestrates everything; standard is directed |
| Prompt routing | Goes to primary agent directly | Goes to orchestrator first for preprocessing |
| Background agent | One sleep-time agent, full LLM | DeepReasoner (high thinking) + orchestrator spawns multiple fast agents |
| Context management | Sleep agent edits memory blocks | Orchestrator formulates targeted context per-agent |
| Self-optimization | None | Orchestrator tracks performance and creates/modifies procedures preemptively |
| Response assembly | Primary agent responds directly | Orchestrator assembles from all sources |
| Cost model | Every sleep cycle = full LLM call | Orchestrator = cheap agents + procedures; deep_reasoner = expensive but selective |

## Implementation in Pares Agens

### Orchestrator → PluresDB Procedures + AgensRuntime
- Autorecall = `before_search` procedure (VectorSearch → Transform → Emit)
- Prompt classification = fast agent or even a classifier procedure
- Performance tracking = `after_store` procedures updating metrics
- Self-optimization = `AgensRuntime` timer that reviews metrics and modifies procedures

### Standard → Standard Agent with Scoped Context
- Receives a constructed prompt from orchestrator (not raw user input)
- Context is pre-filtered and compressed
- Task is well-defined with clear success criteria

### DeepReasoner → High-thinking Agent with PluresDB Triggers
- `on_cue` triggers from orchestrator ("explore this scenario")
- Results written back to PluresDB as typed primitives
- Orchestrator polls or subscribes to results

### Existing Pares Agens Mapping
- `praxis/guidance.rs` `GuidanceService` → orchestrator's output model
- `praxis/ledger.rs` `Ledger` → audit trail for all three agents
- `procedure.rs` `ProcedureRegistry` → orchestrator's procedure management
- `memory/` module → PluresDB integration for all agents
