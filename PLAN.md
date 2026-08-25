# CUA Remaining Production Plan

## Native Backends

1. Implement macOS ScreenCaptureKit capture behind `CaptureBackend`.
2. Implement macOS CGEvent mouse and keyboard input behind `InputBackend`.
3. Add macOS permission probes for Screen Recording, Accessibility/input, Automation, and clipboard.
4. Package the stable signed macOS host app so TCC grants attach to a bundle identity.
5. Implement Windows Graphics Capture and `SendInput` backends with exact integrity-level refusals.
6. Implement Linux X11 capture/input and Wayland portal-mediated capture/input with exact compositor refusals.

## Runtime

1. Add daemon-owned capture, encode, model, input, trace, permission, and event lanes with bounded queues.
2. Add clipboard read/write with explicit grants.
3. Add display, cursor, and window observation from real platform backends.

## Tracing

1. Record action turns with before/after state, before/after images, action JSON, evidence JSON, and session metadata.
2. Add trace replay that re-snapshots and remaps desktop coordinates where needed.
3. Validate trace artifacts with strict schema and evidence checks.
4. Store host-backed evidence under `artifacts/cua/<platform>/`.

## Model Eval

1. Expand eval cases from contract probes to desktop-action tasks with screenshot fixtures and external oracles.

## Performance

1. Add `/metrics` histograms for capture, encode, queue wait, stream send, model send, model response, parse, policy, input dispatch, verification, trace write, and kill-switch propagation.
2. Implement `cua perf live` and `cua perf bench screenshot|stream|input|model-prep`.
3. Enforce latency budgets in local and CI gates.
4. Add memory-growth checks for long-running streams.

## Verification

1. Add host-backed GUI tests for each supported platform/backend/action cell.
2. Add refusal tests that prove no forbidden side effects occurred.
3. Keep `docs/action-support.md` current with proven support, exact refusals, and unproven cells.
