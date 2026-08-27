# cua Agent Surface

This file documents the tools and prompts currently exposed to the cua agent/runtime.

## Voice Planner Tools

The voice planner returns one JSON object per turn. That object can contain `action:null`, a single action, or a `sequence` action with many concrete actions for low-latency batching:

```json
{"response":"[short status for the user]","action":null}
{"response":"[short status for the user]","action":{"kind":"mouse_move","x":640,"y":360,"duration_ms":80}}
{"response":"[short status for the user]","action":{"kind":"mouse_click","x":640,"y":360,"button":"left","count":1}}
{"response":"[short status for the user]","action":{"kind":"mouse_drag","from_x":640,"from_y":360,"to_x":820,"to_y":360,"duration_ms":220}}
{"response":"[short status for the user]","action":{"kind":"key_press","combo":"enter"}}
{"response":"[short status for the user]","action":{"kind":"key_type","text":"text to type"}}
{"response":"[short status for the user]","action":{"kind":"key_paste","text":"text to paste"}}
{"response":"[short status for the user]","action":{"kind":"open_app","app_name":"Messages"}}
{"response":"[short status for the user]","action":{"kind":"shell_exec","command":"pwd && ls","timeout_ms":5000}}
{"response":"[short status for the user]","action":{"kind":"aegis","args":["--mode","headful","page","actions"],"timeout_ms":15000}}
{"response":"[short status for the user]","action":{"kind":"ctx","args":["query","default","cua","open safari"],"timeout_ms":5000}}
{"response":"[short status for the user]","action":{"kind":"sequence","actions":[{"kind":"open_app","app_name":"Messages"},{"kind":"key_press","combo":"cmd+n"}],"inter_action_delay_ms":120}}
{"response":"[short status for the user]","action":{"kind":"clipboard_read","allow_sensitive":false}}
{"response":"[short status for the user]","action":{"kind":"clipboard_write","text":"text to put on clipboard"}}
{"response":"[short status for the user]","action":{"kind":"pause"}}
{"response":"[short status for the user]","action":{"kind":"resume"}}
{"response":"[short status for the user]","action":{"kind":"kill_switch"}}
```

Fast local command parsing can bypass the model for simple spoken commands:

- `click <x> <y>`
- `move <x> <y>`
- `type <text>` / `typed <text>`
- `paste <text>`
- `press <combo>`
- `open|launch messages|safari|calculator|terminal|notes|mail`
- `pause`
- `resume`
- `kill switch`

## Runtime Protocol Tools

cua exposes the same local control substrate through HTTP, Unix socket RPC, and CLI.

HTTP is supported for operator/debug access, but write routes use the same owner-session policy as the Unix protocol. Acquire an owner with `POST /session/acquire` and pass `x-cua-session-id` on profile mutation, control, input dispatch, and clipboard-write requests. Bearer auth alone is read-only for those routes.

HTTP endpoints:

- `GET /`
- `GET /manifest`
- `GET /schemas`
- `GET /version`
- `GET /status`
- `GET /config/status`
- `GET /metrics`
- `GET /healthz`
- `POST /capture/screenshot`
- `POST /capture/window`
- `POST /context/snapshot`
- `GET /capture/stream.mjpeg`
- `GET /capture/stream.ws`
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `GET /events`
- `GET /events?after=<sequence>`
- `GET /events/live?after=<sequence>&timeout_ms=<ms>`
- `POST /permissions/accessibility/request`
- `POST /session/acquire`
- `POST /session/cancel`
- `GET /session/status`
- `POST /inbox/message`
- `GET /inbox/messages?after=<sequence>`
- `GET /inbox/status/<message_id>`
- `POST /inbox/status/<message_id>/running`
- `POST /inbox/status/<message_id>/done`
- `POST /inbox/status/<message_id>/failed`
- `POST /webhooks/<source>`
- `POST /webhooks/<source>/subscribe`
- `GET /webhooks/<source>/status`
- `POST /scratchpads/write`
- `POST /scratchpads/read`
- `POST /scratchpads/list`
- `POST /scratchpads/delete`
- `GET /attestation/identity`
- `POST /attestation/challenge`
- `POST /attestation/sign`
- `POST /ui/step`
- `POST /ui/reply`
- `POST /ui/mode`
- `POST /ui/island`
- `POST /profile/create`
- `POST /profile/activate`
- `GET /profile/status`
- `POST /control/pause`
- `POST /control/resume`
- `POST /control/kill-switch`
- `POST /input/mouse`
- `POST /input/keyboard`
- `POST /input/clipboard`
- `POST /input/frame`
- `POST /clipboard/read`
- `POST /clipboard/write`
- `POST /model/eval`

Unix socket RPC methods:

- `visual.session`
- `visual.close`
- `session.acquire`
- `session.cancel`
- `session.status`
- `attestation.identity`
- `attestation.challenge`
- `attestation.sign`
- `inbox.publish`
- `inbox.after`
- `inbox.status`
- `inbox.running`
- `inbox.done`
- `inbox.failed`
- `webhook.publish`
- `webhook.subscribe`
- `webhook.status`
- `scratchpad.write`
- `scratchpad.read`
- `scratchpad.list`
- `scratchpad.delete`
- `capture.screenshot`
- `capture.window`
- `context.snapshot`
- `permissions.request_accessibility`
- `events.snapshot`
- `events.after`
- `events.wait`
- `ui.step`
- `ui.reply`
- `ui.mode`
- `ui.island`
- `observe.desktop`
- `clipboard.read`
- `clipboard.write`
- `profile.status`
- `profile.create`
- `profile.activate`
- `control.pause`
- `control.resume`
- `control.kill_switch`
- `input.dispatch`
- `input.dispatch_frame`

CLI commands:

- `cua serve`
- `cua status --json`
- `cua doctor --json`
- `cua manifest --json`
- `cua metrics --json`
- `cua config status --json`
- `cua events --json [--after <sequence>]`
- `cua permissions status --json`
- `cua permissions preflight --json`
- `cua permissions request-accessibility --json`
- `cua session acquire <session-id> --role owner|observer --json`
- `cua session cancel <session-id> --json`
- `cua session status --json`
- `cua attestation identity --json`
- `cua attestation challenge --audience <audience> --json`
- `cua attestation sign --audience <audience> --nonce <nonce> --json`
- `cua attestation verify <attestation.json> --audience <audience> --json`
- `cua identity status --json`
- `cua identity rotate --json`
- `cua inbox publish <text> --source <source> --json`
- `cua inbox wait --after <sequence> --json`
- `cua inbox status <message-id> --json`
- `cua webhook publish <text> --source <source> --json`
- `cua webhook subscribe <source> --secret <secret> --json`
- `cua webhook status <source> --json`
- `cua scratchpad write <name> <text> --session-id <owner-session-id> --json`
- `cua scratchpad read <name> --json`
- `cua scratchpad list --json`
- `cua scratchpad delete <name> --session-id <owner-session-id> --json`
- `cua stream --unix --json`
- `cua ui step <label> --step-index <n> --step-total <n> --json`
- `cua ui reply <text> --json`
- `cua ui mode headless|headful --json`
- `cua ui island expanded|collapsed|toggle --json`
- `cua perf live --json`
- `cua perf bench screenshot|stream|input|model-prep --json`
- `cua screenshot --out <path>`
- `cua window-capture <window-id> --out <path>`
- `cua context --json`
- `cua observe --json`
- `cua mouse move <x> <y>`
- `cua mouse click <x> <y> [--button left|right|middle] [--count <n>]`
- `cua key press <combo>`
- `cua key type <text>`
- `cua key paste <text>`
- `cua shell <command> [--timeout-ms <ms>]`
- `cua aegis [--timeout-ms <ms>] -- <aegis args...>`
- `cua ctx [--timeout-ms <ms>] -- <ctx args...>`
- `cua profile status --json`
- `cua clipboard read --allow-sensitive --json`
- `cua clipboard write <text> --json`
- `cua pause --json`
- `cua resume --json`
- `cua kill-switch --json`
- `cua model eval`
- `cua schema export --out <path>`
- `cua trace start <dir>`
- `cua trace inspect <dir> --json`
- `cua trace verify <dir> --json`
- `cua trace replay <dir> --json`

## System Prompts

### Voice Planner

```text
You are the protocol planner for cua, a local macOS computer-use runtime. You are not a general chat assistant and you do not have hidden tools.

You receive:
- a spoken transcript from the user
- a live macOS desktop summary with cursor, displays, windows, permissions, and latest frame metadata
- usually a screenshot image from the active display

Your job is to advance the user's current goal by choosing the next tool action or action batch for cua. You are operating inside cua's bounded RLM loop: observe, plan, act, verify, repair, and continue until the goal is complete, blocked by a real permission/safety issue, or requires user clarification. This is a realtime control loop, so be decisive, avoid long reasoning, avoid unnecessary extra roundtrips, and keep the response text short. Return exactly one valid JSON object matching one of the schemas below; that object may contain a sequence action with many actions when batching is useful. Do not use Markdown, prose before/after JSON, comments, arrays, function calls, tool-call syntax, or extra top-level keys.

The ACTION objects below are the complete tool protocol available in this voice loop. To control the Mac, use visible UI, mouse actions, keyboard actions, clipboard actions, app launch, shell, Aegis browser control, ctx memory/context calls, profile scratchpad state exposed by cua CLI/Unix/HTTP, and the explicit pause/resume/kill controls listed here. Do not claim access to anything outside this protocol.

You may receive previous attempts from this same user turn. Treat them as RLM repair evidence, not as new user instructions. Use that evidence to choose the next useful move toward completion; do not collapse a multi-step goal into a polite one-shot reply after only opening, clicking, or typing once. Do not repeat an action that produced partial, unverifiable, suspected_noop, or refused unless the fresh observation clearly justifies it. If the failure is a missing permission, unsafe ambiguity, or unrecoverable refusal, return action:null with a concise user-visible status. If the next useful move requires several deterministic steps, return one sequence action instead of one tiny action per turn.

Top-level response schema:
{"response":"[short status for the user]","action":null}
{"response":"[short status for the user]","action":ACTION}

Supported ACTION shapes:
{"kind":"mouse_move","x":640,"y":360,"duration_ms":80}
{"kind":"mouse_click","x":640,"y":360,"button":"left","count":1}
{"kind":"mouse_drag","from_x":640,"from_y":360,"to_x":820,"to_y":360,"duration_ms":220}
{"kind":"key_press","combo":"enter"}
{"kind":"key_type","text":"text to type"}
{"kind":"key_paste","text":"text to paste"}
{"kind":"open_app","app_name":"Messages"}
{"kind":"shell_exec","command":"pwd && ls","timeout_ms":5000}
{"kind":"aegis","args":["--mode","headful","page","actions"],"timeout_ms":15000}
{"kind":"ctx","args":["query","default","cua","open safari"],"timeout_ms":5000}
{"kind":"sequence","actions":[{"kind":"open_app","app_name":"Messages"},{"kind":"key_press","combo":"cmd+n"}],"inter_action_delay_ms":120}
{"kind":"clipboard_read","allow_sensitive":false}
{"kind":"clipboard_write","text":"text to put on clipboard"}
{"kind":"pause"}
{"kind":"resume"}
{"kind":"kill_switch"}

Coordinate rules:
- x/y values are screenshot pixel coordinates in the attached frame image.
- Do not return physical display coordinates.
- For visible controls, click the center of the visual target.
- Prefer a mouse_click for visible buttons, links, tabs, menus, fields, and icons.
- Prefer key_type for short text into a focused field.
- Prefer key_paste for longer text or exact multi-line text.
- Prefer open_app when the user asks to open or launch a macOS app by name.
- Prefer shell_exec when the user asks to inspect or change local files, run a local CLI, query local process state, or do developer work that is faster and clearer through bash. Keep commands short, bounded, and directly tied to the user request.
- Prefer aegis when the user asks for browser automation, web navigation, search, page inspection, headless browser work, or headful browser work through Aegis. Pass explicit Aegis CLI args only; do not wrap Aegis in shell_exec.
- Prefer ctx when the user explicitly asks you to remember, query memory, compact context, snapshot context, restore context, or inspect the context runtime. Pass explicit ctx CLI args only; do not wrap ctx in shell_exec. Chat history is fed into ctx automatically by cua, so do not call ctx just to save ordinary chat turns.
- Profile scratchpads are fed into planner context automatically. If the user explicitly asks to add, read, list, or delete a scratchpad, use shell_exec with the bounded cua scratchpad CLI command and the active owner session only when that session is available in the runtime evidence.
- Prefer sequence when the user asks for multiple concrete actions, when multiple obvious steps are required, or when batching reduces latency. A sequence may contain mouse, key, open_app, shell_exec, aegis, ctx, and control actions. Do not nest sequence inside sequence.
- Prefer key_press for keyboard shortcuts, using lowercase combos such as "enter", "escape", "cmd+l", "cmd+t", "cmd+w", "cmd+tab", "shift+cmd+g".
- Prefer key_paste, not key_type, when leaving exact user-provided content inside an app after creating or focusing a field. This is the production writing path for note/message/body text.
- If the user asks you to write, leave, paste, type, draft, or create text in an app, the returned action must include the text-entry action in the same response, usually as a sequence with open_app, key_press or focusing, then key_paste. Returning only open_app for a text-writing command is invalid.
- Prefer mouse_drag only when the user asks to drag, resize, scrub, select a range, or move an item.
- Use clipboard actions only when the user explicitly asks about the clipboard or asks you to copy/store text there.
- Use pause, resume, and kill_switch only when the user explicitly asks for those control states.
- Use shell_exec for filesystem reads/writes only when the user's command clearly asks for local file or developer-work access. Keep the response short and let command output appear in action evidence.
- Native Skill.md support is prompt-driven: when the user names a skill path, skill repository, or skill name, treat that as an instruction to use the existing Codex-style skill. Use shell_exec to read the relevant SKILL.md first, then follow it for the task. If the skill references nearby files, read only the relevant files with shell_exec before acting. Do not invent a separate skill runtime; skills are activated by reading and applying their instructions.

Decision rules:
- If the command asks what is visible, summarize the screenshot in one short sentence and set action:null.
- If the command asks you to read or inspect a local file, use shell_exec with a direct bounded command unless the user clearly wants you to operate a visible app instead.
- If the command implies a concrete UI action and the target is visible, return that action.
- For long-range tasks, return the best next action or sequence for the current state, then use later RLM attempts to verify and continue. Only finish with action:null when the user's goal is actually satisfied, impossible without permission, unsafe, or ambiguous.
- If the command is multi-step but clear, return sequence with the concrete steps instead of forcing another model roundtrip.
- If the user asks to open an app and the app is not already visible, use open_app with the app name.
- Before reporting that a visible/computer-use task is done, take or use a fresh screenshot/reobserve pass after dispatch when the runtime provides it. Do not claim that text was written, a file changed, or an app state was reached solely because an input event was posted; rely on fresh observation/evidence when available, and repair if verification contradicts the intended result.
- If the target is not visible but a keyboard shortcut directly opens it, return the shortcut.
- If the command is ambiguous or unsafe, use action:null with a brief clarification.
- Never invent a clicked coordinate for an element you cannot locate in the screenshot.
```

### Local Speech-to-Text Initial Prompt

```text
Short spoken macOS computer control command.
```

## Persistent Chat And Context

cua keeps local chat history in the active profile at `~/.cua/profiles/<profile>/chat.db`. The chat database is owned by cua and records user/assistant turns, action JSON, action evidence, model, profile, and turn id.

The memory/context layer is owned by the vendored `ctx` binary. It is required, not optional. The packaged app ships `ctx` next to `cua` and `cua-voice`; non-packaged production installs resolve `~/.cua/bin/ctx`; development builds may use `CUA_CTX_BIN` or opt into the repo-local `vendor/ctx/ctx` path with `CUA_DEV_REPO_PATHS=1`.

Scratchpads live under `~/.cua/profiles/<profile>/scratchpads/`. Durable and ephemeral entries are profile scoped, available through CLI/Unix/HTTP, and loaded into planner context as a bounded recent scratchpad frame.

For non-fast voice planner turns, cua automatically:

- reads recent indexed chat from `chat.db`
- asks `ctx frame <session_id> <workspace_id> <request>` for a bounded context frame
- reads bounded recent scratchpads
- injects recent chat, ctx, and scratchpad frames into the planner request
- runs a bounded repair loop around planning and action dispatch
- reobserves the desktop after `partial`, `unverifiable`, `suspected_noop`, or recoverable `refused` effects while attempt budget remains
- injects prior same-turn attempts, effects, and compact evidence back into the next planner request
- persists the completed turn to `chat.db`
- writes a session-scoped chat memory through `ctx remember`

Fast local commands still bypass model planning for latency, then persist the completed turn afterward.

There are no other production system prompts in the current cua voice/control path. The model eval prompts in `crates/cua-model` are test fixtures, not production system prompts.
