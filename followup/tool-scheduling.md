# Add bounded conflict-aware tool scheduling

Implemented; see `docs/agents.md` for the scheduling and nested-dispatch contract.

Implement within Animist. Current execution is sequential and pending results are
persisted before execution. Preserve that default and the Layer/AgentLayer list.
Craft's dispatcher is a reference for conflict-aware waves, not code to transplant.

## Goal

Add explicit opt-in bounded concurrency without tool-name heuristics.

## Requirements

- Let tools or an application-owned policy report exclusivity/resource conflicts.
  Unknown policy must remain exclusive.
- Bound tasks, child fan-out, and total tool calls across nested dispatch.
- Child/meta-tools must use the same authorization, cancellation, and result
  pipeline; acquiring a permit for a parent must not deadlock its children.
- Preserve deterministic model result ordering and pending-result revision safety.
- Define declaration visibility separately from registration. Layers may narrow
  the advertised set, but neither model calls nor child dispatch may bypass it.
- Do not retry side-effecting tools. Specify timeout and cancellation uncertainty.
- Keep workspace path normalization, plan modes, snapshots, and permission UI in
  consuming applications, not Animist.

## Validation

Test bounded concurrency, conflicting writes, unknown-policy serialization, nested
dispatch, denial/stop propagation, cancellation/drop, stable result ordering, and
no orphan tool results. Run cargo test and cargo check --all-features.
