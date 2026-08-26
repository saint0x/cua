# cua Eval Results

Last bounded live run: 2026-08-25.

Configuration:

- Provider: OpenRouter chat completions
- Calls: 20
- Candidates: 4
- Cases per candidate: 5
- Max output tokens per call: 160
- Temperature: 0
- Screenshots: generated desktop-action fixtures

Results from `artifacts/cua/smoke/model-eval-broader-live.json`:

| Model | Oracle score | Average latency |
| --- | ---: | ---: |
| `openai/gpt-5.4-mini` | 1.00 | 1206 ms |
| `google/gemini-3.5-flash-lite` | 0.66 | 1671 ms |
| `google/gemini-3.7-flash` | 0.40 | 3513 ms |
| `openai/gpt-5-mini` | 0.40 | 4460 ms |

Current winner: `openai/gpt-5.4-mini`.

Reason: it satisfied all generated desktop-action fixture oracles exactly after catalog validation confirmed the candidate supports image input and text output. The broader run covered center-button click, focused text entry, top-right toolbar click, sidebar-row click, and focused search-field typing.

Qualitative result: typing tasks were easy for all candidates that returned valid JSON, but click targeting separated the models. `openai/gpt-5.4-mini` returned exact action JSON and coordinates for every click task. The weaker candidates either produced malformed JSON, truncated before a complete action, or clicked plausible but wrong coordinates.

Usage smoke: `artifacts/cua/smoke/model-eval-live-usage.json` confirmed provider token usage and finish reasons are captured with `--max-calls 2 --max-output-tokens 128`.
