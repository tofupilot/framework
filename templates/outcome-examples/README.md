# Outcome Examples

Comprehensive demonstration of all 8 outcome cases from the UX design document.

## Cases Covered

| # | Python Code | PhaseResult | Outcome | NextAction | Behavior |
|---|-------------|-------------|---------|------------|----------|
| 1 | `phase.retry()` | RETRY | fail | retry (fail if limit hit) | Bumps retry count and retries phase |
| 2 | `return` (implicit) | CONTINUE | pass | continue | Default happy path - continues to next phase |
| 3 | `phase.fail()` | FAIL | fail | depends on on_first_failure | Test failed - triggers stop if configured |
| 4 | timeout | _ | timeout | stop (overridable) | Killed by timeout |
| 5 | exception | ERROR | error | stop | Unexpected exception |
| 6 | `phase.stop()` | STOP | stop | stop | User-initiated stop |
| 7 | `phase.skip()` | SKIP | skip | continue | Skips to next phase |
| 8 | measurement fails | CONTINUE | fail | depends on on_first_failure | Measurement outside validator range |

## Key Learnings

- **Default behavior**: No return = CONTINUE → pass (if measurements valid)
- **Overrides**: Use `then.*` to override default next actions
- **Priority**: timeout/error override user PhaseResult
- **Retry limits**: Retry converts to fail when limit exceeded
- **Measurements**: Critical validator failures override CONTINUE → fail
