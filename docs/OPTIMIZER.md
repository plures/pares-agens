# Optimizer — Max-Min Optimization Engine

> **Runtime:** `pares-agens-optimizer` (`crates/optimizer`)
> **Control-plane:** remains in `praxis-business`

## Overview

`pares-agens-optimizer` is a native max-min optimization lane for Pares Agens.  It allows fine-tuned model policies to perform optimization decisions natively, without depending on external solvers.

### Max-min philosophy

Given a set of policy dimensions (one score per agent or sub-policy), max-min optimization finds the policy that **maximises the worst-case (minimum) score**.  This is the standard fairness objective in multi-agent / robust RL literature.

```
maximise  min_i { w_i · score_i }
subject to  constraint_j(x) ≥ lower_bound_j  ∀ j
```

---

## Architecture

```
OptimizerInput
  └── run_id, policy_id, objective (scores + weights),
      constraints, max_iterations, convergence_tolerance, context
           │
           ▼
  MaxMinOptimizer::run()
     ├── Policy::step()   ← pluggable; default: DefaultPolicy
     ├── Objective::evaluate()
     ├── Constraint::is_satisfied()
     └── TelemetryEmitter::emit()
           │
           ▼
  OptimizationResult
    └── objective_score, iterations, converged, violated_constraints
```

---

## Modules

| Module | Key type | Purpose |
|--------|----------|---------|
| `lib` | `OptimizerInput`, `OptimizationResult`, `Objective`, `Constraint` | Core data types and error enum |
| `engine` | `MaxMinOptimizer`, `Policy`, `DefaultPolicy` | Iterative optimizer + evaluation hooks |
| `telemetry` | `TelemetryEmitter`, `ObservabilityEvent` | Structured observability |
| `benchmark` | `BenchmarkHarness`, `BenchmarkReport` | Baseline vs optimized comparison |

---

## Plugging in a Fine-Tuned Model Policy

Implement the `Policy` trait from `pares_agens_optimizer::engine`:

```rust
use pares_agens_optimizer::{Constraint, OptimizerError};
use pares_agens_optimizer::engine::Policy;

pub struct MyFineTunedPolicy;

impl Policy for MyFineTunedPolicy {
    fn step(
        &self,
        iteration: u32,
        current_scores: &[f64],
        constraints: &[Constraint],
    ) -> Result<Vec<f64>, OptimizerError> {
        // Call your fine-tuned model here; return updated scores.
        // The returned Vec must have the same length as current_scores.
        todo!()
    }
}
```

Wire it up:

```rust
use pares_agens_optimizer::engine::MaxMinOptimizer;
use pares_agens_optimizer::telemetry::TelemetryEmitter;

let optimizer = MaxMinOptimizer::with_policy(
    TelemetryEmitter::noop(),
    Box::new(MyFineTunedPolicy),
);
let result = optimizer.run(input)?;
```

---

## Telemetry

The emitter fires `ObservabilityEvent` variants at each lifecycle point:

| Event | When |
|-------|------|
| `EpisodeStarted` | Once at run start; includes initial score and context |
| `IterationCompleted` | After each optimizer step; includes score and improvement delta |
| `ConstraintViolated` | Whenever a constraint is violated; lists constraint names |
| `EpisodeCompleted` | On run end; includes final score, convergence status, violations |

All events implement `Serialize` — forward them to your tracing/metrics backend:

```rust
let emitter = TelemetryEmitter::new(|event| {
    tracing::info!(event = ?event, "optimizer telemetry");
});
```

---

## Offline / Online Evaluation Hooks

```rust
// Offline: evaluate over a fixed dataset before deployment
let mean_score = pares_agens_optimizer::engine::offline_evaluate(
    &episodes,
    &my_policy,
    1e-4,   // convergence_tolerance
    50,     // max_iterations
)?;

// Online: single-episode evaluation in the live loop
let score = pares_agens_optimizer::engine::online_evaluate(
    input,
    TelemetryEmitter::noop(),
)?;
```

---

## Benchmark

```rust
use pares_agens_optimizer::benchmark::{BenchmarkConfig, BenchmarkHarness};

let config = BenchmarkConfig {
    baseline_step_size: 0.0,   // no improvement — raw model output
    optimized_step_size: 0.3,  // tuned step
};
let harness = BenchmarkHarness::new(config);
let report = harness.run(episodes)?;
println!("{}", serde_json::to_string_pretty(&report)?);
// Artifact: { baseline_mean_score, optimized_mean_score, absolute_improvement, … }
```

The `BenchmarkReport` is a JSON-serialisable artifact that can be committed to CI as a reproducible benchmark result.

---

## Boundary Notes

- **Runtime optimization logic** lives here in `pares-agens-optimizer`.
- **Orchestration / control-plane** (when to run optimization, routing decisions) remains in `praxis-business`.
- **Policy boundaries**: `Policy::step` is the only seam between the optimizer loop and model-specific logic; keep side effects out of the optimizer core.
