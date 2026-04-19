# ADR-0008: Self-Improving Analysis-Research-Experiment Loop

**Status:** Accepted\
**Date:** 2026-03-24\
**Author:** mswork (automated)\
**Context:** ADR-0007 (Epistemic Integrity)

## Context

After the automation-infrastructure incident (ADR-0007), we have confidence-scored facts
and anomaly detection. But these are passive — they tell us what's wrong but don't fix it.

Praxis needs to actively improve itself:

1. Analyze its own state (where are the gaps?)
2. Generate research questions (what should we find out?)
3. Run experiments (test hypotheses in sandbox)
4. Apply results (update facts, confidence, rules)
5. Loop (new analysis reveals new gaps)

Design Dojo's lab pattern (draft→testing→graduating→archived) provides the lifecycle model.
Chronos provides the temporal tracking. The uncertainty module provides the evidence framework.

## Decision

### New Modules

1. **Analysis** (`src/analysis/`) — self-introspection
   - Fact coverage (known vs. expected domains)
   - Confidence distribution with propagation anomalies
   - Rule effectiveness (fire rates, dormant rules, noop rules)
   - Dependency health (cycles, critical chains, orphans)
   - Prediction accuracy with calibration curves

2. **Research** (`src/research/`) — question generation
   - Auto-generates from analysis gaps
   - Prioritized by impact × feasibility
   - Hypotheses for every question
   - Tracks completion through to findings

3. **Experiments** (`src/experiments/`) — sandboxed testing
   - Four experiment kinds: fact-verification, rule-modification, model-calibration, A/B comparison
   - Full sandbox with resource limits, timeouts, isolation levels
   - Results produce evidence for the uncertainty module
   - Rule changes tested in isolation before production

4. **Integration Hub** (`src/integration/hub.ts`) — the loop
   - Connects analysis → research → experiments → evidence → analysis
   - Auto-approves low-risk experiments
   - Tracks predictions for calibration
   - System health dashboard
   - Chronos integration for temporal tracking

### Self-Improvement Capabilities

- **Rule experiments**: Proposed rule modifications are tested in sandboxed Praxis engines
  before being applied to production. A/B comparisons measure improvement.
- **Model calibration**: Experiments test the assigned LLM against known inputs, measuring
  accuracy, hallucination rate, and consistency. Results feed back to inform prompt design.
- **Prediction tracking**: Every confident claim becomes a testable prediction. Over time,
  calibration curves show whether 80% confidence predictions are right 80% of the time.

### Constraints

- Experiments MUST run in sandbox (no production mutation)
- High-resource experiments REQUIRE approval
- Model calibration experiments are budget-capped
- Results are logged but never auto-applied without review (for rule/model changes)
- Fact-verification experiments can auto-apply confidence updates

## Consequences

- Praxis becomes self-aware of its own knowledge gaps
- Research questions drive focused improvement
- Experiments provide evidence for uncertain facts
- Model calibration improves the LLM Praxis works with
- The loop is bounded by resource budgets — it can't spiral

## Risks

- Self-improvement loops can be recursive — resource limits prevent runaway
- Model calibration relies on "known good" test cases which may themselves be wrong
- Auto-approved experiments might produce misleading results at scale

## Related

- ADR-0007: Epistemic Integrity (uncertainty module, anomaly detection)
- Design Dojo Lab Pattern (lifecycle model)
- Chronos (temporal tracking)
