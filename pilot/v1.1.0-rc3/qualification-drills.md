# RC3 qualification drills

RC1 and RC2 evidence are immutable. These RC3 drills use deterministic
fixtures and the new image; none imports either historical candidate.

| Drill | Case | Expected result |
| --- | --- | --- |
| `contained_evaluator_toolchain_complete` | RC3 image with the FD, HTTPX, and Zod declared prerequisite sets | PASS before any agent execution |
| `contained_evaluator_toolchain_complete` | RC2 image evaluated against the RC3 FD prerequisite set | doctor fails closed on missing `cargo-clippy`; no agent executes |
| `contained_evaluator_toolchain_complete` | A declared executable becomes unavailable at evaluator start | `EvaluatorToolUnavailable`, evaluator `INCONCLUSIVE/error`, never engineering FAIL |
| `failed_workspace_retained_when_configured` | Completed engineering FAIL with `keep_on_failure = true` | workspace exists and a durable `WorkspaceDispositionRecorded(kept)` event is present |
| `failed_workspace_retained_when_configured` | PASS with ordinary cleanup | durable evidence remains, workspace is absent, and the disposition is `removed` |
| `failed_workspace_retained_when_configured` | Cleanup reports an error or leaves the path present | typed `WorkspaceCleanupFailed` and disposition `cleanup_failed`; Forge never records removed |

The retention unit matrix additionally covers PASS, FAIL, INCONCLUSIVE,
infrastructure ERROR, no-change, timeout, and cancellation with
`keep_on_failure` both enabled and disabled.
