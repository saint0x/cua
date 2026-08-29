# Local HTTP API

cua exposes the CLI and local HTTP API for operator access. Latency-sensitive voice control and SDK mutation paths use the profile-local Unix socket at `~/.cua/profiles/<profile>/daemon.sock`.

The daemon is backed by a general computer backend. The default installed backend is the local macOS computer, selected through the backend-neutral runtime boundary and attested with the local machine identity. Remote CUA daemons, Oracle VM-hosted computers, and later Quilt VM fleets plug in as alternate computer backends that implement the same capture, observe, input, permission, clipboard, session, safety, trace, and attestation contract.

Default bind: `127.0.0.1:8765`.

Security:

- `cua serve` refuses non-loopback binds unless `--allow-lan` is passed.
- Authenticated endpoints require `Authorization: Bearer <token>`.
- Production profile tokens are loaded from `~/.cua/profiles/<profile>/http.token`.
- `CUA_HTTP_TOKEN` is honored only for tests or explicit development runs with `CUA_DEV_HTTP_TOKEN_OVERRIDE=1`.
- The Unix socket uses the same profile token and newline-delimited JSON requests.
- HTTP write routes require an active owner session. Acquire one with `POST /session/acquire`, then send `x-cua-session-id: <owner-session-id>` on profile mutation, control, input dispatch, and clipboard-write requests.
- Scratchpad write/delete routes also require `x-cua-session-id`; scratchpad read/list routes are authenticated read-only context access.
- Observer sessions are valid for reads such as status, manifest, schemas, screenshot, context, observe, events, and visual sessions. Observer sessions cannot mutate runtime state.
- Unix write callers pass `session_id` in the request envelope.
- `GET /`, `GET /version`, and `GET /healthz` are unauthenticated readiness/discovery endpoints.
- HTTP remains an operator/debug surface; SDK hot paths should prefer the profile-local Unix socket.

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
- `POST /capture/window`
- `POST /context/snapshot`
- `GET /capture/stream.mjpeg`: continuous MJPEG stream from the daemon capture lane, newest-frame-per-tick, no backlog
- `GET /capture/stream.ws`: continuous WebSocket stream from the daemon capture lane with JSON frame envelopes and binary JPEG frames
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `GET /events`: retained events from the bounded daemon event lane
- `GET /events?after=<sequence>`: retained events after a sequence number
- `GET /events/live?after=<sequence>&timeout_ms=<ms>`: bounded long-poll event wait from the daemon event lane
- `POST /permissions/accessibility/request`
- `POST /ui/step`: publish an agent-programmed visible HUD step with `label`, optional `task`, `tool`, `source`, `step_index`, `step_total`, and `ttl_ms`
- `POST /ui/reply`: publish an agent-programmed visible HUD reply flash with `text`, optional `source`, and `ttl_ms`
- `POST /ui/mode`: switch a running HUD between `headful` and `headless` without changing the underlying computer-use control path
- `POST /session/acquire`
- `POST /session/heartbeat`
- `POST /session/cancel`
- `GET /session/status`
- `POST /inbox/message`: inject an authenticated message into the running agent loop
- `GET /inbox/messages?after=<sequence>`: poll retained inbound messages after an inbox sequence
- `GET /inbox/status/<message_id>`: read message processing state
- `POST /inbox/status/<message_id>/running`
- `POST /inbox/status/<message_id>/done`
- `POST /inbox/status/<message_id>/failed`
- `POST /webhooks/<source>`: publish an inbound message for a webhook source
- `POST /webhooks/<source>/subscribe`: configure source secret and optional reply URL
- `GET /webhooks/<source>/status`
- `POST /scratchpads/write`: create, replace, or append a profile scratchpad
- `POST /scratchpads/read`: read one profile scratchpad by name
- `POST /scratchpads/list`: list durable and ephemeral profile scratchpads
- `POST /scratchpads/delete`: delete one scratchpad name from durable and/or ephemeral storage
- `GET /attestation/identity`
- `POST /attestation/challenge`
- `POST /attestation/sign`
- `POST /profile/create`
- `POST /profile/activate`
- `GET /profile/status`
- `POST /control/pause`
- `POST /control/resume`
- `POST /control/kill-switch`
- `POST /input/dispatch`: backend-neutral `InputAction` dispatch endpoint used by remote computer providers
- `POST /input/mouse`
- `POST /input/keyboard`
- `POST /input/clipboard`
- `POST /input/frame`
- `POST /clipboard/read`
- `POST /clipboard/write`
- `POST /model/eval`

Webhook source secrets are optional for local development, but when a source has a secret configured, `POST /webhooks/<source>` requires `x-cua-webhook-signature: sha256=<hmac>` over the raw request body. The request body is an `InboundMessageRequest`; `idempotency_key` deduplicates repeat delivery.

Oracle VM node control is shipped through the normal backend contract rather than a parallel cloud API: `GET /status` and `GET /session/status` report `computer_backend`, including `kind`, `provider`, `runtime`, optional cloud/fleet identifiers, operating system, and backend capability manifest. Durable fleet enrollment routes for pool membership, provider rotation, and revocation are separate control-plane work.

Backend selection is explicit. With no environment override, `cua serve` uses the local macOS backend. Set `CUA_COMPUTER_BACKEND=remote_cua` with `CUA_REMOTE_CUA_URL` and `CUA_REMOTE_CUA_TOKEN` to proxy a remote CUA daemon over HTTP. Set `CUA_COMPUTER_BACKEND=oracle-vm` on an Oracle VM node to use the bundled `qgui+cua` Linux backend. Set `CUA_COMPUTER_BACKEND=quilt-vm` on a Quilt VM node to use the same internal qgui implementation with Quilt provider identity. Optional `CUA_REMOTE_CUA_INSTANCE_ID`, `CUA_REMOTE_CUA_POOL_ID`, `CUA_REMOTE_CUA_REGION`, and `CUA_REMOTE_CUA_OS` values are reported in backend identity. Missing backend requirements produce an unavailable backend with no advertised capture/input capabilities, and unavailable capture/observe paths return HTTP 503 instead of pretending cloud control is ready.

`GET /status` reports `active_streams`; stream clients increment the count on connect and decrement after disconnect cleanup. Its top-level `computer_backend` and `inventory.computer_backend` identify the selected computer substrate. Its `inventory.config` object reports canonical `~/.cua` paths and migration state without exposing bearer token contents.

`GET /config/status` returns that same config inventory directly for SDKs, runebooks, and scripts that need to discover the profile socket, profile root, chat database, ctx workspace, scratchpad root, trace roots, and config migration state.

Scratchpads:

- Scratchpads are stored under `~/.cua/profiles/<profile>/scratchpads/`, split into `durable` and `ephemeral` entry folders.
- Names are single path segments containing only letters, numbers, dot, dash, and underscore.
- `ScratchpadWriteRequest` supports `durable`, `append`, and `ttl_ms`; durable entries ignore TTL, ephemeral entries expire and are pruned on scratchpad reads/lists/writes.
- The voice planner receives a bounded recent scratchpad context frame automatically.

`GET /metrics` returns typed latency histograms and counters for hot runtime paths including screenshot capture, encode queueing, streaming ticks, input dispatch, model queueing, permission probes, trace writes, clipboard operations, emitted stream frames, refusals, active streams, dropped events, dropped encode jobs, dropped model jobs, permission fallbacks, and dropped trace jobs. `cua perf live --json` reads the same endpoint. `cua perf bench screenshot|stream|input|model-prep --json` runs bounded local latency checks against the daemon and fails when p95 exceeds its budget.

Profile and safety:

- `POST /session/acquire` accepts `SessionLeaseRequest`. Use `role = "owner"` before calling any write endpoint.
- `POST /session/heartbeat` accepts `SessionHeartbeatRequest` and renews an active owner or observer lease.
- `POST /profile/create` installs an inactive in-daemon policy with mode, expiry, and capability manifest. Requires owner.
- `POST /profile/activate` activates the current profile unless the daemon generation has been killed. Requires owner.
- `POST /control/pause` pauses automation. Requires owner.
- `POST /control/resume` resumes automation unless kill-switch is active. Requires owner.
- `POST /control/kill-switch` is terminal for the current daemon generation. Requires owner.

Clipboard:

- `POST /clipboard/write` accepts `ClipboardWriteRequest` and writes through the selected computer backend only when the active profile grants `capabilities.clipboard` and the caller supplies an owner lease.
- `POST /clipboard/read` accepts `ClipboardReadRequest` and reads through the selected computer backend only when the active profile grants clipboard and `allow_sensitive` is true.
- Clipboard calls are refused while paused, killed, inactive, or ungranted, with refusal evidence in `ClipboardResult.result`.
