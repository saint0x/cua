# Model Eval

`cua model eval` compares candidate still-frame reasoning models through OpenRouter with a hard call cap and low output-token budget. Live runs validate candidates against the OpenRouter model catalog before inference. Output includes raw case results, token usage when the provider returns it, per-model summaries, failure classifications, and a winner chosen by average contract score, error count, then average latency.

Defaults are dry-run only. Live provider calls require either `--live` or `CUA_MODEL_EVAL_LIVE=1`.

Cost controls:

- `--max-calls`
- `--max-output-tokens`
- `CUA_MODEL_EVAL_MAX_CALLS`
- `CUA_MODEL_EVAL_MAX_TOKENS`

Current candidates:

- `google/gemini-3.5-flash-lite`
- `google/gemini-3.7-flash`
- `openai/gpt-5.4-mini`
- `openai/gpt-5-mini`

The first eval cases are deliberately tiny coordinate/action-contract probes so cost stays bounded while the harness matures.
