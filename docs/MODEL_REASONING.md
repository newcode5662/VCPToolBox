# Model reasoning controls (v1)

## Boundary

- Authenticated `GET /v1/model-capabilities?model=<public-id>` returns
  `{version:1, model, reasoning:{version,status,source,efforts,supportsBudget,canDisable,defaultEffort,defaultEnabled}}`.
- UI consumes this normalized schema, not vendor model names or wire fields.
- Requests carry `vcpchatExtensions.reasoning`:
  `{version:1,mode:"default"}`, `{version:1,mode:"effort",value:"high"}`, or
  `{version:1,mode:"budget",value:4096}`.
- Extensions are stripped at the backend. Existing timestamp extensions coexist.
- Default leaves legacy request behavior unchanged. Explicit settings replace legacy
  thinking fields. Unsupported explicit selections fail with a visible error, never
  silently change to a different cost/effort.
- Every network retry resolves the actual fallback model before applying the setting.
  The same request-local wrapper serves stream and nonstream tool continuations.

## Discovery

Only explicit `ModelReasoning` metadata is parsed:
`reasoning.supported_efforts`, `mandatory`, `default_effort`,
`default_enabled`, `supports_max_tokens`. Missing metadata is unknown.
A null supported_efforts allowlist has the documented OpenRouter meaning of all
gateway efforts. A supported_parameters name alone does not prove allowed values.

This metadata schema uses the `reasoning` wire adapter. Other protocols require an
exact backend override; no model-name heuristics. Dynamic virtual routing models
without a precise capability override remain unknown in the UI.

Catalog loads are single-flight, capped at 8 MiB and 10 seconds, cached for 60 seconds.
A failed refresh cannot authorize an explicit choice using stale data.

## Exact overrides

Create `model-reasoning.json` at the VCPToolBox root (not required by default):

```json
{
  "version": 1,
  "models": {
    "exact-internal-model-id": {
      "adapter": "reasoning_effort",
      "supported_efforts": ["low", "medium", "high"],
      "mandatory": true
    },
    "exact-budget-model-id": {
      "adapter": "thinking",
      "supports_max_tokens": true
    }
  }
}
```

Use the **internal ID after ModelRedirect**, not the displayed alias.
Supported adapters:
- `reasoning`: `reasoning.effort` / `reasoning.max_tokens`
- `reasoning_effort`: effort only
- `thinking`: `thinking.type=enabled` and `thinking.budget_tokens` (budget only)
- `thinking_budget`: `thinking_config.thinking_budget` (budget only)

Declare only values accepted by the configured gateway, not merely the underlying
provider. Overrides are read for each lookup; malformed files fail explicitly.
Budget must be a positive safe integer and below max_tokens when present.
Provider-specific minimum/maximum budgets are not inferred: configure/select a
budget accepted by the gateway; provider rejection remains visible.

## Activation and compatibility

Restart backend and VCPChat to load main-process/preload changes. No automatic
restart, deployment or billable probe is performed by this implementation.
Old backends return no capabilities, so UI remains at default. Explicit controls
require a new backend: do not downgrade the backend while keeping saved explicit
settings without resetting them to default.

Tests use offline fixtures. They do not certify a particular live gateway, provider
budget limit, current model alias, or dynamic route.