# Sample cua Runebook

This file is a design reference for the proposed `cua run <file>.toml` runebook system.

The runebook is not implemented yet. It should be implemented in full as the canonical compact scripting surface over the existing cua daemon, Unix socket protocol, HTTP API, voice/text/wav planner path, STT path, traces, profile/session policy, model calls, and future machine attestation.

The intended command:

```sh
cua run ./task.cua.toml
cua run ./task.cua.toml --attest --profile default
```

## Design Goal

A cua runebook is a compact structured program that compiles to the full cua protocol.

It should support:

1. Direct deterministic protocol steps.
2. Sequential steps.
3. Batched input.
4. Parallel steps.
5. Race/wait logic.
6. Macros.
7. Child runebooks.
8. Model calls.
9. Text turns.
10. WAV turns.
11. Live recording turns.
12. STT.
13. Planner dispatch.
14. UI/HUD events.
15. Clipboard.
16. Aegis.
17. ctx.
18. Trace verify/replay.
19. Perf/model eval.
20. Attestation and cloud enrollment.

## Complete Example

```toml
schema = "cua.runebook.v1"

[run]
name = "quilt-docs-agent"
profile = "default"
mode = "supervised" # observe | supervised | autonomous
timeout_ms = 120000
on_error = "stop" # stop | continue | ask | rollback
trace = true

[daemon]
ensure = true
addr = "127.0.0.1:8765"
hud = "headful" # headful | headless | off
allow_lan = false

[session]
role = "owner" # owner | observer
client = "quilt-docs-runebook"
ttl_ms = 300000

[attest]
required = true
audience = "quilt-cloud"
save_as = "machine_attestation"

[stt.default]
backend = "local" # local | openrouter
model = "tiny.en"
fallback_model = "base.en"
timeout_ms = 15000
attempts = 3
retry_backoff_ms = 180

[planner.default]
provider = "openrouter"
model = "google/gemini-2.5-flash-lite"
timeout_ms = 12000
attempts = 3
retry_backoff_ms = 220
max_tokens = 180
fast_commands = true
response_format = "json_action"

[memory]
chat = true
ctx = true
workspace = "profile"

[trace]
dir = "~/.cua/profiles/default/traces/runebooks/quilt-docs-agent"
verify_on_complete = true

[vars]
url = "https://docs.quilt.sh"
question = "Open Quilt docs and inspect the page actions."

[macro.open_url]
items = [
  { do = "open_app", app = "Safari" },
  { do = "key", combo = "cmd+l" },
  { do = "type", text = "$url" },
  { do = "key", combo = "enter" },
]
delay_ms = 80

[[steps]]
id = "health"
do = "status"
save_as = "status"

[[steps]]
id = "preflight"
do = "permissions"
action = "preflight" # status | preflight | request_accessibility
save_as = "permissions"

[[steps]]
id = "attest"
do = "attest"
audience = "quilt-cloud"
save_as = "attestation"

[[steps]]
id = "show-progress"
do = "ui.step"
label = "Opening Quilt docs"
source = "runebook"
task = "Docs inspection"
tool = "Safari"
step_index = 1
step_total = 6
ttl_ms = 5000

[[steps]]
id = "open-docs"
do = "macro.open_url"
url = "${url}"

[[steps]]
id = "parallel-state"
do = "parallel"
limit = 3
items = [
  { do = "context", max_width = 1280, include_bytes = true, force_fresh = true, save_as = "ctx" },
  { do = "events", save_as = "events" },
  { do = "metrics", save_as = "metrics" },
]

[[steps]]
id = "ask-built-in-planner"
do = "turn"
input_kind = "text" # text | wav | record
input = "${question}"
stt = "default"
planner = "default"
context = true
memory = true
dispatch = true
save_as = "planner_turn"

[[steps]]
id = "ask-generic-model"
do = "model"
provider = "openrouter"
model = "google/gemini-2.5-flash-lite"
system = "Return a cua runebook fragment as JSON."
message = "Given $ctx and $events, decide the next useful inspection step."
include_context = ["$ctx", "$events"]
response = "json"
save_as = "decision"

[[steps]]
id = "maybe-dispatch-model-action"
do = "dispatch_model_action"
from = "$decision.action"
frame = "$ctx.frame.envelope"
if_present = true
save_as = "model_dispatch"

[[steps]]
id = "finish"
do = "ui.reply"
text = "Quilt docs opened and inspected."
ttl_ms = 3000

[[steps]]
id = "verify-trace"
do = "trace.verify"
dir = "$trace.dir"
```

## Top-Level Fields

### `schema`

Required schema version.

```toml
schema = "cua.runebook.v1"
```

### `[run]`

Runtime policy for the whole runebook.

```toml
[run]
name = "example"
profile = "default"
mode = "supervised"
timeout_ms = 120000
on_error = "stop"
trace = true
```

`on_error` is proposed and not implemented today. Required enum:

- `stop`: stop execution immediately.
- `continue`: record the error and continue to the next step.
- `ask`: pause and ask the user/operator what to do.
- `rollback`: run rollback handlers or trace replay rollback hooks where available.

### `[daemon]`

Daemon startup and HUD behavior.

```toml
[daemon]
ensure = true
addr = "127.0.0.1:8765"
hud = "headful"
allow_lan = false
```

### `[session]`

Owner/observer session behavior.

```toml
[session]
role = "owner"
client = "runebook"
ttl_ms = 300000
```

### `[attest]`

Machine attestation behavior.

```toml
[attest]
required = true
audience = "quilt-cloud"
save_as = "machine_attestation"
```

### `[stt.<name>]`

Speech-to-text settings.

```toml
[stt.default]
backend = "local"
model = "tiny.en"
fallback_model = "base.en"
timeout_ms = 15000
attempts = 3
retry_backoff_ms = 180
```

Current source supports:

- local Whisper STT
- OpenRouter STT
- local fallback model
- timeout env controls
- retry attempts
- retry backoff
- English language
- WAV input

### `[planner.<name>]`

Planner/model settings for cua action planning.

```toml
[planner.default]
provider = "openrouter"
model = "google/gemini-2.5-flash-lite"
timeout_ms = 12000
attempts = 3
retry_backoff_ms = 220
max_tokens = 180
fast_commands = true
response_format = "json_action"
```

Current source supports OpenRouter planner calls, a strict JSON action response, fast-command bypass, screenshot image input, desktop summary, chat memory, ctx memory, retry attempts, retry backoff, and timeouts.

### `[memory]`

Local chat and ctx behavior.

```toml
[memory]
chat = true
ctx = true
workspace = "profile"
```

Current source stores chat in the profile and uses a profile-local ctx workspace.

### `[trace]`

Trace behavior.

```toml
[trace]
dir = "~/.cua/profiles/default/traces/runebooks/example"
verify_on_complete = true
```

## Step Vocabulary

The runebook executor should expose every current cua control surface.

### Runtime And Discovery

```toml
[[steps]]
do = "status"

[[steps]]
do = "doctor"

[[steps]]
do = "manifest"

[[steps]]
do = "schemas"

[[steps]]
do = "metrics"
```

### Permissions

```toml
[[steps]]
do = "permissions"
action = "status"

[[steps]]
do = "permissions"
action = "preflight"

[[steps]]
do = "permissions"
action = "request_accessibility"
```

### Sessions

```toml
[[steps]]
do = "session.acquire"
session_id = "owner-1"
role = "owner"
client_name = "runebook"
ttl_ms = 300000

[[steps]]
do = "session.cancel"
session_id = "owner-1"
target_session_id = "observer-1"

[[steps]]
do = "session.status"
```

### Profiles

```toml
[[steps]]
do = "profile.create"
name = "automation"
mode = "supervised"
duration_ms = 60000
clipboard = true

[[steps]]
do = "profile.activate"

[[steps]]
do = "profile.status"
```

### Capture And Observation

```toml
[[steps]]
do = "context"
max_width = 1280
encoding = "png"
force_fresh = true
include_bytes = true
save_as = "ctx"

[[steps]]
do = "screenshot"
max_width = 1280
encoding = "png"
force_fresh = true
include_bytes = true
out = "~/.cua/profiles/default/screenshots/screen.png"

[[steps]]
do = "window_capture"
window_id = 123
max_width = 1280
encoding = "png"
out = "~/.cua/profiles/default/screenshots/window.png"

[[steps]]
do = "observe"
target = "desktop" # desktop | displays | cursor

[[steps]]
do = "visual"
fps = 10
max_width = 1280
include_bytes = false
frames = 3
save_as = "frames"
```

### Events And Waits

```toml
[[steps]]
do = "events"
after = 0
save_as = "events"

[[steps]]
do = "wait_event"
after = "$events.max_sequence"
kind = "ui_step"
timeout_ms = 5000

[[steps]]
do = "wait_agent_step"
after = 0
timeout_ms = 5000

[[steps]]
do = "wait_agent_reply"
after = 0
timeout_ms = 5000
```

### UI And HUD

```toml
[[steps]]
do = "ui.step"
label = "Working"
source = "runebook"
task = "Local automation"
tool = "cua"
step_index = 1
step_total = 3
ttl_ms = 5000

[[steps]]
do = "ui.reply"
text = "Done."
source = "runebook"
ttl_ms = 3000

[[steps]]
do = "ui.mode"
mode = "headless"
source = "runebook"

[[steps]]
do = "ui.island"
state = "toggle" # expanded | collapsed | toggle
source = "runebook"
```

### Input Actions

Canonical raw input:

```toml
[[steps]]
do = "input"
action = { kind = "mouse_click", x = 420, y = 240, button = "left", count = 1 }

[[steps]]
do = "input.frame"
source_frame = "$ctx.frame.envelope"
action = { kind = "mouse_click", x = 420, y = 240, button = "left", count = 1 }
```

Compact aliases:

```toml
[[steps]]
do = "mouse_move"
x = 5
y = 5
duration_ms = 0

[[steps]]
do = "click"
x = 420
y = 240
button = "left"
count = 1

[[steps]]
do = "drag"
from_x = 100
from_y = 100
to_x = 400
to_y = 400
duration_ms = 220

[[steps]]
do = "key"
combo = "cmd+l"

[[steps]]
do = "type"
text = "hello"

[[steps]]
do = "paste"
text = "long exact text"

[[steps]]
do = "open_app"
app = "Safari"

[[steps]]
do = "shell"
cmd = "pwd && ls"
timeout_ms = 5000

[[steps]]
do = "aegis"
args = ["--mode", "headful", "page", "actions"]
timeout_ms = 15000

[[steps]]
do = "ctx"
args = ["query", "default", "cua", "open safari"]
timeout_ms = 5000
```

### Clipboard

Clipboard must remain explicit because current source intentionally routes clipboard through profile-gated endpoints.

```toml
[[steps]]
do = "clipboard.write"
text = "hello"

[[steps]]
do = "clipboard.read"
allow_sensitive = true
save_as = "clipboard"
```

### Control

```toml
[[steps]]
do = "pause"

[[steps]]
do = "resume"

[[steps]]
do = "kill_switch"
```

## Model, STT, Planner, And Turn Steps

### Built-In Text Turn

Runs the existing cua planner path with a programmatic text transcript.

```toml
[[steps]]
id = "text-turn"
do = "turn"
input_kind = "text"
input = "Open Safari and go to ${url}."
planner = "default"
context = true
memory = true
dispatch = true
save_as = "turn"
```

### WAV Turn

Runs WAV -> STT -> planner -> optional dispatch.

```toml
[[steps]]
id = "wav-turn"
do = "turn"
input_kind = "wav"
wav = "~/.cua/profiles/default/uploads/command.wav"
stt = "default"
planner = "default"
dispatch = true
save_as = "voice_turn"
```

### Live Recording Turn

Records local audio, transcribes it, plans, and optionally dispatches.

```toml
[[steps]]
id = "record-turn"
do = "turn"
input_kind = "record"
record_ms = 4500
stt = "default"
planner = "default"
dispatch = true
save_as = "record_turn"
```

### STT Only

```toml
[[steps]]
id = "transcribe"
do = "stt"
backend = "local"
model = "tiny.en"
wav = "~/.cua/profiles/default/uploads/input.wav"
save_as = "transcript"
```

### Planner Only

```toml
[[steps]]
id = "plan"
do = "planner"
planner = "default"
transcript = "$transcript.text"
context = "$ctx"
memory = true
dispatch = false
save_as = "plan"
```

### Generic Model Call

This is not limited to the built-in voice planner. It lets a runebook send a message to a model at a specific step.

```toml
[[steps]]
id = "decide-next"
do = "model"
provider = "openrouter"
model = "google/gemini-2.5-flash-lite"
system = "Return a cua runebook fragment as JSON."
message = "Given $ctx and $events, decide the next useful inspection step."
include_context = ["$ctx", "$events"]
response = "json"
save_as = "decision"
```

### Dispatch Model Output

```toml
[[steps]]
do = "dispatch_model_action"
from = "$decision.action"
frame = "$ctx.frame.envelope"
if_present = true
```

### Spawn Child Run From Model Output

```toml
[[steps]]
do = "spawn_run"
from = "$decision.runebook"
mode = "child"
wait = true
save_as = "child_result"
```

### Run Another Runebook File

```toml
[[steps]]
do = "run"
file = "./subtask.cua.toml"
vars = { url = "${url}" }
wait = true
save_as = "subtask"
```

## Workflow Logic

### Sequential Group

```toml
[[steps]]
do = "seq"
items = [
  { do = "open_app", app = "Safari" },
  { do = "key", combo = "cmd+l" },
  { do = "type", text = "${url}" },
  { do = "key", combo = "enter" },
]
delay_ms = 80
```

### Parallel Group

```toml
[[steps]]
do = "parallel"
limit = 3
items = [
  { do = "context", max_width = 1280, save_as = "ctx" },
  { do = "events", save_as = "events" },
  { do = "metrics", save_as = "metrics" },
]
```

### Race Group

```toml
[[steps]]
do = "race"
items = [
  { do = "wait_event", kind = "ui_reply", timeout_ms = 10000 },
  { do = "sleep", ms = 3000 },
]
save_as = "winner"
```

### Batch Input

```toml
[[steps]]
do = "batch"
mode = "input_sequence"
delay_ms = 80
actions = [
  { kind = "key_press", combo = "cmd+l" },
  { kind = "key_type", text = "${url}" },
  { kind = "key_press", combo = "enter" },
]
```

### Foreach

```toml
[[steps]]
do = "foreach"
items = ["https://quilt.sh", "https://docs.quilt.sh"]
as = "url"
steps = [
  { do = "turn", input_kind = "text", input = "Open ${url} and summarize what is visible.", dispatch = true },
]
```

## Trace, Perf, Model Eval, Schema

```toml
[[steps]]
do = "trace.start"
dir = "~/.cua/profiles/default/traces/runebooks/example"

[[steps]]
do = "trace.inspect"
dir = "$trace.dir"

[[steps]]
do = "trace.verify"
dir = "$trace.dir"

[[steps]]
do = "trace.replay"
dir = "$trace.dir"
dry_run = true

[[steps]]
do = "perf.bench"
target = "screenshot" # screenshot | stream | input | model_prep
iterations = 5
warmup = 1
budget_ms = 5000

[[steps]]
do = "model.eval"
live = false
max_calls = 4
max_output_tokens = 256

[[steps]]
do = "schema.export"
out = "~/.cua/artifacts/schema/schema-bundle.json"
```

## Attestation And Enrollment

These are proposed features and are not implemented in the current source.

```toml
[[steps]]
do = "identity.status"

[[steps]]
do = "attest"
audience = "quilt-cloud"
save_as = "attestation"

[[steps]]
do = "enroll"
audience = "quilt-cloud"
attestation = "$attestation"
save_as = "enrollment"
```

## Escape Hatch

Every runebook should support raw protocol RPC for newly added daemon methods.

```toml
[[steps]]
do = "rpc"
transport = "unix"
method = "input.dispatch"
params = { kind = "key_press", combo = "enter" }
save_as = "raw_result"
```

## Required Implementation Notes

1. Add a runebook parser and validator.
2. Add `cua run <file>` to the CLI.
3. Add typed runebook structs, probably in `cua-core` or a new `cua-runebook` crate.
4. Compile compact aliases to existing cua protocol calls.
5. Support raw RPC escape hatch.
6. Support `save_as` result binding.
7. Support `${var}` interpolation.
8. Support `$result.path` references.
9. Support `seq`, `parallel`, `race`, `batch`, and `foreach`.
10. Support child runebook spawning.
11. Support model calls that can return actions or child runebooks.
12. Support built-in text/wav/record turns through the existing voice planner path.
13. Implement `on_error = stop|continue|ask|rollback`.
14. Write traces for runebook execution.
15. Verify traces and support replay.
16. Add fozzy coverage for deterministic runebook execution.

