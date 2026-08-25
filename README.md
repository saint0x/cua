# CUA

CUA is a CLI-first local computer-use runtime for agents. It provides one resident daemon, a local HTTP API, typed desktop observation, action delivery results, trace artifacts, and bounded model evals for selecting vision/action models without turning the project into an agent framework.

Public control surfaces are intentionally limited to:

- `cua` CLI
- loopback local HTTP API

## Structure

- `crates/cua-core`: shared protocol types and JSON schema bundle
- `crates/cua-cli`: human-facing CLI
- `crates/cua-daemon`: resident local control plane and HTTP API
- `crates/cua-capture`: frame bus and capture backend contract
- `crates/cua-input`: input backend contract and typed action results
- `crates/cua-model`: OpenRouter eval harness and model selection report
- `crates/cua-trace`: trace records and validation helpers
- `crates/cua-platform-*`: platform backend boundaries
- `docs/`: API, eval, and support notes
- `fozzy/`: deterministic scenario verification

## Run

```sh
cargo run -p cua -- serve --addr 127.0.0.1:8765
cargo run -p cua -- status --json
cargo run -p cua -- screenshot --out artifacts/cua/smoke/screen.png --json
```

## Evaluate Models

Create a local `.env`:

```sh
OPENROUTER_API_KEY=...
CUA_MODEL_EVAL_LIVE=0
CUA_MODEL_EVAL_MAX_TOKENS=256
CUA_MODEL_EVAL_MAX_CALLS=8
```

Dry run:

```sh
cargo run -p cua -- model eval --max-calls 4 --json
```

Bounded live run:

```sh
cargo run -p cua -- model eval --live --max-calls 8 --json
```

## Verify

```sh
cargo fmt --check
cargo test
cargo check
cargo build -p cua
fozzy doctor --deep --scenario fozzy/scenarios/cua-smoke.json --runs 5 --seed 4242 --json
fozzy test --det --strict-verify fozzy/scenarios/cua-smoke.json --json
```

For trace verification:

```sh
mkdir -p artifacts/cua/fozzy
fozzy run fozzy/scenarios/cua-smoke.json --det --record artifacts/cua/fozzy/cua-smoke.fozzy --json
fozzy trace verify artifacts/cua/fozzy/cua-smoke.fozzy --strict --json
fozzy replay artifacts/cua/fozzy/cua-smoke.fozzy --json
fozzy ci artifacts/cua/fozzy/cua-smoke.fozzy --json
```

## Status

The current runtime has production-shaped contracts, daemon/CLI plumbing, synthetic capture, refusal-only input, schema export, trace inspection, and bounded model evals. Real platform capture/input backends and signed host installation are still in progress.

