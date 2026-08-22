# RC3 exact-image evaluator substrate qualification

- Image: `localhost:5000/forge/pilot-runtime@sha256:ede1b16e60b242ddb5edd00a32327cb4ba535b08ba08cef16a3715e05f296104`
- Platform: `linux/arm64`
- Image ID: `sha256:44df4327bc26b291ac8dc78253e40fd49258ef4f35a34cf7bcb12b190f2151c5`
- Execution: credential-free, read-only root filesystem, private executable
  `/tmp`, writable disposable baseline clone, ordinary Docker bridge, 2 CPUs,
  4 GiB memory, 256 PIDs.
- Shell: the production evaluator path, `/bin/sh -c`.
- Model calls: none.

Every distinct frozen evaluator command was executed at the exact repository
baseline. A nonzero result is acceptable here only when it is the intended
baseline engineering signal and the evaluator itself ran correctly.

| Command set | Baseline result | Qualification |
| --- | --- | --- |
| FD `cargo test --locked` | 268 passed, exit 0 | runnable |
| FD rustfmt + Clippy | exit 0 | runnable; RC2 missing tools present |
| FD overflow contract | exit 1 | runnable; expected baseline task signal |
| HTTPX Trio write-timeout | 1 pass, 1 ResourceWarning failure | runnable; expected baseline task signal |
| HTTPX response/header link suites | 133 passed, exit 0 | runnable |
| HTTPX authentication suite | 8 passed, exit 0 | runnable |
| HTTPX `scripts/check` | format, mypy, and Ruff passed | runnable |
| Zod focused Vitest/type tests | 202 passed, exit 0 | runnable |
| Zod focused Biome check | 2 files clean, exit 0 | runnable |

Pre-freeze probing caught and corrected two RC3 task-copy defects: the HTTPX
link task referenced nonexistent `tests/test_models.py`, and the lint plans
attempted to interpret the shell script `scripts/check` as Python. The frozen
commands now target the actual model suites and put `.pilot-venv/bin` on PATH
before executing the script. Their corrected task bytes are the only bytes
listed in the manifest and the fresh shadow-routing stores.

An earlier probe used `/bin/sh -lc`, which is not Forge's evaluator launch
path; Debian's login profile omitted `/usr/local/cargo/bin` and produced three
discarded exit-127 results. The production `/bin/sh -c` path was verified
directly and is the path used for every result above.
