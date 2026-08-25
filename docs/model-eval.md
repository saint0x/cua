# Model Eval

`cua model eval` compares candidate still-frame reasoning models through OpenRouter with a hard call cap and low output-token budget. Live runs validate candidates against the OpenRouter model catalog before inference. Output includes raw case results, token usage when the provider returns it, per-model summaries, failure classifications, and a winner chosen by oracle score, error count, then average latency.

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

Default cases use generated desktop screenshot fixtures with external oracles for action kind, coordinates, typed text, and coordinate tolerance. This keeps the live run bounded while still testing visual desktop-action behavior.
