# CUA Remaining Production Plan

## Native Backends

1. Upgrade macOS capture lane to ScreenCaptureKit streaming capture.
2. Package the stable signed macOS host app so TCC grants attach to a bundle identity.

## Runtime

1. Add daemon-owned encode, model, trace, and permission lanes with bounded queues.

## Tracing

1. Store host-backed evidence under `artifacts/cua/<platform>/`.

## Model Eval

1. Expand eval cases from contract probes to desktop-action tasks with screenshot fixtures and external oracles.

## Verification

1. Add host-backed GUI tests for macOS backend/action cells.
2. Keep `docs/action-support.md` current with proven support, exact refusals, and unproven cells.
