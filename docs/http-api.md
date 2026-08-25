# Local HTTP API

CUA is driven only by the CLI and local HTTP API.

Default bind: `127.0.0.1:8765`.

Security:

- `cua serve` refuses non-loopback binds unless `--allow-lan` is passed.
- Authenticated endpoints require `Authorization: Bearer <token>`.
- Profile tokens are loaded from `CUA_HTTP_TOKEN` or `~/.cua/profiles/<profile>/http.token`.
- `GET /`, `GET /version`, and `GET /healthz` are unauthenticated readiness/discovery endpoints.

Initial endpoints:

- `GET /`
- `GET /manifest`
- `GET /schemas`
- `GET /version`
- `GET /status`
- `GET /metrics`
- `GET /healthz`
- `POST /capture/screenshot`
- `GET /capture/stream.mjpeg`: continuous MJPEG stream, newest-frame-per-tick, no backlog
- `GET /capture/stream.ws`: continuous WebSocket stream with JSON frame envelopes and binary JPEG frames
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `GET /events`
- `GET /events/live`
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

`GET /status` reports `active_streams`; stream clients increment the count on connect and decrement after disconnect cleanup.

`GET /metrics` returns typed latency histograms and counters for hot runtime paths including screenshot capture, streaming ticks, input dispatch, clipboard operations, emitted stream frames, refusals, and active streams. `cua perf live --json` reads the same endpoint.

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
