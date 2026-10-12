# Optimization
Use only for an explicit /optimize request to measure and improve software resource use: CPU, allocations, peak memory, latency or throughput. Do not optimize system prompts, skills, instructions, tool selection or model behavior.

Source changes require separately permitted Codebase Admin access and an isolated broker task. Call performance_work for baseline measurements, supported CPU/allocation profiles and candidate comparisons. Use the effective saved settings and validated command overrides; the broker enforces every budget. Report unsupported profiling honestly and stop on exhausted budgets.

Measure the original workload before proposing changes. Keep timing and per-child peak memory measurements separate from profiler overhead. Use representative coverage, adverse/concurrent cases when configured, and full correctness checks for baseline and candidates. Compare every candidate with both the original and best accepted result; reject regressions beyond configured limits. Shared-host contention makes timing conclusions provisional.

Do not add dependencies unless the effective dependency scope permits them and policy admits the operation. Do not weaken correctness checks to fit a budget. Retain receipts and artifacts. Finish with a verified candidate and measured report for review; do not deploy or release the candidate automatically.
