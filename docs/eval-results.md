# CUA Eval Results

Last bounded live run: 2026-08-25.

Configuration:

- Provider: OpenRouter chat completions
- Calls: 8
- Candidates: 4
- Cases per candidate: 2
- Max output tokens per call: 256
- Temperature: 0
- Screenshot: synthetic 640 px PNG

Results from `artifacts/cua/smoke/model-eval-live.json`:

| Model | Contract score | Average latency |
| --- | ---: | ---: |
| `google/gemini-3.5-flash-lite` | 0.70 before fenced-JSON normalization | 1291 ms |
| `google/gemini-3.7-flash` | 1.00 | 1985 ms |
| `openai/gpt-5.4-mini` | 1.00 | 907 ms |
| `openai/gpt-5-mini` | 1.00 | 3986 ms |

Current winner: `openai/gpt-5.4-mini`.

Reason: it satisfied both action-contract probes exactly and had the lowest average latency among exact-contract models.

Note: the scorer now normalizes fenced JSON, so `google/gemini-3.5-flash-lite` should be rerun before making a durable production decision.

