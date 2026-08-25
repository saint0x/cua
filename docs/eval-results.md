# CUA Eval Results

Last bounded live run: 2026-08-25.

Configuration:

- Provider: OpenRouter chat completions
- Calls: 8
- Candidates: 4
- Cases per candidate: 2
- Max output tokens per call: 128
- Temperature: 0
- Screenshots: generated desktop-action fixtures

Results from `artifacts/cua/smoke/model-eval-fixture-live.json`:

| Model | Oracle score | Average latency |
| --- | ---: | ---: |
| `openai/gpt-5.4-mini` | 1.00 | 1040 ms |
| `google/gemini-3.5-flash-lite` | 0.50 | 1551 ms |
| `google/gemini-3.7-flash` | 0.50 | 2735 ms |
| `openai/gpt-5-mini` | 0.50 | 3013 ms |

Current winner: `openai/gpt-5.4-mini`.

Reason: it satisfied both generated desktop-action fixture oracles exactly after catalog validation confirmed the candidate supports image input and text output.

Usage smoke: `artifacts/cua/smoke/model-eval-live-usage.json` confirmed provider token usage and finish reasons are captured with `--max-calls 2 --max-output-tokens 128`.
