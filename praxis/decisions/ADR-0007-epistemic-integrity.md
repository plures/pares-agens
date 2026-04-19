# ADR-0007: Epistemic Integrity — Uncertainty Propagation and Anomaly Detection

## Status: Accepted

## Date: 2026-03-24

## Context

On 2026-03-24, we discovered `plures/automation-infrastructure` — a shadow automation system
that had been operating undetected across 46 repositories for 23 days. It had:

- 10 active workflows (PR Lane FSM, Merge Gate, CI Escalation, Auto-Assign, etc.)
- A "PR Lane Event Relay" deployed to 46 repos, funneling all PR/review/check events
- 190 workflow runs in the last 24 hours alone
- Its own issue assignment system competing with our `copilot-pr-lifecycle.yml`

**Root cause**: We made assumptions and stated them as facts. We assumed we knew all actors
in the system. We assumed all workflow runs we observed were caused by our pipelines. When
Copilot was assigned to issues and created PRs, we attributed behaviors to our automation
that may have been caused by the rogue system. Every conclusion built on a false premise
was potentially wrong.

**The dangerous pattern**: Praxis treats facts as binary — true or false. But in a system
with autonomous agents, facts have _confidence levels_. An observation ("Copilot was assigned
to issue #4") might have multiple explanations (our queue-advance, automation-infrastructure's
auto-assign, or manual assignment). Without tracking confidence, we build fragile logic chains.

## Decision

### 1. Facts carry confidence scores (0.0–1.0)

Every fact stored in the system carries:

- `confidence`: 0.0 (pure speculation) to 1.0 (directly verified)
- `source`: what produced this fact (observation, inference, assumption)
- `evidence`: array of supporting observations
- `contraEvidence`: array of contradicting observations

### 2. Uncertainty propagates through inference chains

When Fact B depends on Fact A:

- `confidence(B) ≤ confidence(A) × confidence(B_given_A)`
- If A's confidence drops, all downstream facts are automatically flagged

### 3. Anomaly detection via org-wide workflow census

Periodic census of all repos comparing:

- Known workflows (deployed by us) vs. observed workflows
- Expected actors vs. actual actors
- Workflow run volume per repo (statistical outliers)

### 4. Praxis expectations declare what SHOULD exist

For every fact we depend on, we declare an expectation:

```
expect('only-queue-advance-assigns-copilot')
  .onlyWhen('queue-advance workflow runs')
  .never('from unknown workflows')
  .always('trackable to a known trigger')
```

Violations surface as alerts, not silent corruption.

### 5. Chronos tracks causal chains

Every action in the pipeline creates a chronos node with causal links.
If we observe an effect without a cause in our chain, that's an anomaly.

## Consequences

- Facts that were previously binary become probability-weighted
- The system can say "I'm 60% confident this is true" instead of asserting certainty
- Unknown actors are detected by comparing expected vs. observed behavior
- Build decisions automatically degrade when underlying facts lose confidence
- Increased complexity in the fact storage layer, but massive increase in epistemic safety
