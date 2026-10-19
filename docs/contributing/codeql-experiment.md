# CodeQL Latency Experiment

`CodeQL Latency Experiment` compares two threads with all available cores on standard
Ubuntu runners. It leaves the required default CodeQL scan unchanged. The workflow runs
three samples per setting when its own file changes, or when dispatched manually. No
larger runner or paid capacity is configured.

Every sample checks out the same revision and uses the same pinned CodeQL action, linked
CLI/query bundle, default Rust queries, and memory limit. Project dependency and TRAP
caches are disabled for comparability; the hosted image's preinstalled tool cache may
still be used. Results therefore describe this experiment's cache state, not every pull
request.

Download the six `codeql-experiment-*` artifacts from one completed run. Check
`source.json`, `cpu.json`, and `outcome.json` before comparing samples: source revision,
bundle, runner image, cores, memory, and success must match. Inspect the SARIF for the
same rules, source coverage, extraction diagnostics, and findings. Failed or incomplete
runs are evidence, not faster samples.

Use the Actions job/step timestamps to separate queue, setup, and analysis. The CodeQL
logs identify extraction, database finalization, and query execution within analysis.
Report medians and ranges across all three samples, preserve outliers, and keep runner
work separate from elapsed feedback time. Peak memory is not measured by this workflow.

The experiment retains SARIF as an artifact for comparison, but publishes neither
security results nor databases to code scanning. Changing required scan configuration
needs a separate, evidence-backed change after the experiment.
