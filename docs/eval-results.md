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

Results from `artifacts/cua/smoke/model-eval-live-catalog.json`:

| Model | Contract score | Average latency |
| --- | ---: | ---: |
| `openai/gpt-5.4-mini` | 1.00 | 964 ms |
| `google/gemini-3.7-flash` | 1.00 | 2153 ms |
| `openai/gpt-5-mini` | 1.00 | 3044 ms |
| `google/gemini-3.5-flash-lite` | 0.65 | 969 ms |

Current winner: `openai/gpt-5.4-mini`.

Reason: it satisfied both action-contract probes exactly and had the lowest average latency among exact-contract models after catalog validation confirmed the candidate supports image input and text output.
