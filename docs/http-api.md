# Local HTTP API

cua exposes the CLI and local HTTP API for operator access. Latency-sensitive voice control uses the profile-local Unix socket at `~/.cua/profiles/<profile>/daemon.sock`.

Default bind: `127.0.0.1:8765`.

Security:

- `cua serve` refuses non-loopback binds unless `--allow-lan` is passed.
- Authenticated endpoints require `Authorization: Bearer <token>`.
- Production profile tokens are loaded from `~/.cua/profiles/<profile>/http.token`.
- `CUA_HTTP_TOKEN` is honored only for tests or explicit development runs with `CUA_DEV_HTTP_TOKEN_OVERRIDE=1`.
- The Unix socket uses the same profile token and newline-delimited JSON requests.
- HTTP write routes require an active owner session. Acquire one with `POST /session/acquire`, then send `x-cua-session-id: <owner-session-id>` on profile mutation, control, input dispatch, and clipboard-write requests.
- `GET /`, `GET /version`, and `GET /healthz` are unauthenticated readiness/discovery endpoints.

Initial endpoints:

- `GET /`
- `GET /manifest`
- `GET /schemas`
- `GET /version`
- `GET /status`
- `GET /config/status`
- `GET /metrics`
- `GET /healthz`
- `POST /capture/screenshot`
- `GET /capture/stream.mjpeg`: continuous MJPEG stream from the daemon capture lane, newest-frame-per-tick, no backlog
- `GET /capture/stream.ws`: continuous WebSocket stream from the daemon capture lane with JSON frame envelopes and binary JPEG frames
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `GET /events`: retained events from the bounded daemon event lane
- `GET /events/live?after=<sequence>&timeout_ms=<ms>`: bounded long-poll event wait from the daemon event lane
- `POST /ui/step`: publish an agent-programmed visible HUD step with `label`, optional `task`, `tool`, `source`, `step_index`, `step_total`, and `ttl_ms`
- `POST /ui/reply`: publish an agent-programmed visible HUD reply flash with `text`, optional `source`, and `ttl_ms`
- `POST /ui/mode`: switch a running HUD between `headful` and `headless` without changing the underlying computer-use control path
- `POST /profile/create`
- `POST /profile/activate`
- `GET /profile/status`
- `POST /control/pause`
- `POST /control/resume`
- `POST /control/kill-switch`
- `POST /input/mouse`
- `POST /input/keyboard`
- `POST /input/clipboard`
- `POST /clipboard/read`
- `POST /clipboard/write`
- `POST /model/eval`

`GET /status` reports `active_streams`; stream clients increment the count on connect and decrement after disconnect cleanup. Its `inventory.config` object reports canonical `~/.cua` paths and migration state without exposing bearer token contents.

`GET /config/status` returns that same config inventory directly for SDKs, runebooks, and scripts that need to discover the profile socket, profile root, chat database, ctx workspace, trace roots, and config migration state.

`GET /metrics` returns typed latency histograms and counters for hot runtime paths including screenshot capture, encode queueing, streaming ticks, input dispatch, model queueing, permission probes, trace writes, clipboard operations, emitted stream frames, refusals, active streams, dropped events, dropped encode jobs, dropped model jobs, permission fallbacks, and dropped trace jobs. `cua perf live --json` reads the same endpoint. `cua perf bench screenshot|stream|input|model-prep --json` runs bounded local latency checks against the daemon and fails when p95 exceeds its budget.

Profile and safety:

- `POST /profile/create` installs an inactive in-daemon policy with mode, expiry, and capability manifest.
- `POST /profile/activate` activates the current profile unless the daemon generation has been killed.
- `POST /control/pause` pauses automation.
- `POST /control/resume` resumes automation unless kill-switch is active.
- `POST /control/kill-switch` is terminal for the current daemon generation.

Clipboard:

- `POST /clipboard/write` accepts `ClipboardWriteRequest` and writes to the daemon-owned clipboard store only when the active profile grants `capabilities.clipboard`.
- `POST /clipboard/read` accepts `ClipboardReadRequest` and returns clipboard text only when the active profile grants clipboard and `allow_sensitive` is true.
- Clipboard calls are refused while paused, killed, inactive, or ungranted, with refusal evidence in `ClipboardResult.result`.
