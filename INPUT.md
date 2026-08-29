# Cloud Computer Input Backend Plan

This document describes the next build step for making cloud computer use real. The current CUA backend layer is modular: local macOS is the default installed backend, remote CUA can proxy another daemon, and Oracle Cloud Infrastructure can launch/status/terminate VM capacity. The production cloud computer backend is Oracle VM now, with qgui bundled inside the Oracle VM node image as its GUI session manager. Quilt VM later reuses the same node image pattern with a different provider identity.

`qgui` is allowed to be vendored locally and used end to end.

Reference inspected:

- Repo: `https://github.com/ariacomputecompany/qgui`
- Commit inspected: `5db59870775108d8fe4768ffdc7cd4c2b74cebfc`
- Contract file: `/run/qgui/session.json`
- Runtime: KasmVNC + XFCE + dbus

## Target Shape

The cloud computer should be a normal `ComputerBackend`, not a side runtime.

Default install:

```text
cua serve
  -> CUA_COMPUTER_BACKEND unset
  -> local macOS backend
  -> local machine attestation
```

Oracle VM node install:

```text
Oracle VM node image
  -> qgui up
  -> cua serve --hud-mode headless
  -> CUA_COMPUTER_BACKEND=oracle-vm
  -> Oracle VM backend uses the bundled qgui session internally for capture, observe, input, clipboard, and app launch
```

Remote control plane:

```text
operator / agent
  -> fleet provider acquires VM
  -> remote CUA endpoint
  -> same /capture, /observe, /input/dispatch, /session, /status protocol
```

Provider transition:

```text
Oracle VM now
  -> cheap/free-credit capacity
  -> prove value and reliability

Quilt VM later
  -> same internal qgui + cua node image
  -> provider swap only
  -> no agent/runtime protocol rewrite
```

## Vendor qgui

Vendor `qgui` under one of these forms:

- Preferred: `vendor/qgui/` as source, built as a workspace member or invoked as a bundled binary.
- Acceptable: copy its contract crate/module into a new local crate if we want tighter API boundaries.

Required vendored surface:

- `qgui up`
- `qgui down`
- `qgui status`
- `qgui env --format json`
- `qgui run -- <command...>`
- `/run/qgui/session.json`

The CUA code should not scrape human CLI text if it can read `session.json` or JSON output.

## qgui Fixes Before Production

Patch these either upstream or in the vendored copy:

1. Default bind should be `127.0.0.1`, not `0.0.0.0`.
2. Add `qgui status --format json` or `qgui status --json`.
3. Track and stop `dbus-daemon`; current health mainly tracks KasmVNC.
4. Make session/auth files explicitly private:
   - `/run/qgui/session.json`: `0600` if it includes backend password
   - `/root/.kasmpasswd`: `0600`
   - `/run/qgui`: `0700`
5. Replace the clippy failure: avoid `format!` inside `println!`.
6. Add integration tests for:
   - missing binaries
   - stale X lock
   - session JSON parse
   - env JSON
   - `qgui run` receiving `DISPLAY`, dbus, and runtime dir

## Oracle VM Node qgui Implementation Crate

Add an internal Oracle VM node GUI implementation crate:

```text
crates/cua-platform-qgui/
```

Responsibilities:

- Load the qgui session contract.
- Verify qgui health.
- Implement the `ComputerBackend` surface used by `ComputerBackendKind::OracleVm`.
- Provide capture backend.
- Provide input backend.
- Provide qgui app/shell launch.
- Report honest capabilities only when bundled qgui and bundled X input/capture tooling are healthy.

Descriptor:

```rust
ComputerBackendDescriptor {
    kind: ComputerBackendKind::OracleVm,
    provider: "oracle-vm",
    runtime: "qgui+cua",
    os: "linux",
    capabilities: ...
}
```

Provider-specific overlays should still identify the capacity provider:

- Oracle VM node: `kind = OracleVm`, `provider = "oracle-vm"`, `runtime = "qgui+cua"`
- Quilt node: `kind = QuiltVm`, `provider = "quilt-vm"`, `runtime = "qgui+cua"`

## Backend Selection

Extend daemon backend selection:

```text
CUA_COMPUTER_BACKEND=oracle-vm
CUA_COMPUTER_BACKEND=quilt-vm
```

Expected behavior:

- `oracle-vm`: run only on the Oracle VM node image and require healthy bundled qgui.
- `quilt-vm`: same internal qgui node implementation, different provider identity.
- qgui is not a public backend selector. It is the internal GUI runtime for cloud node backends.
- If bundled qgui or bundled X tooling is missing or unhealthy, return degraded/missing permissions and 503 on capture/observe. Never fabricate frames or input success.

## Capture Backend

Implement real Linux capture from qgui’s X display.

Preferred order:

1. Direct X11 capture through `x11rb`, `xcb`, or equivalent Rust bindings.
2. `grim`/Wayland path only if qgui moves away from X11.
3. No distro CLI fallback in production. Any helper must be vendored/built as a workspace artifact.

Requirements:

- Capture full display.
- Return truthful `DisplayInfo`.
- Encode PNG/JPEG through existing `cua-capture` path.
- Never fabricate frames.
- Preserve frame dimensions and display origin.
- Time out cleanly.
- Report unavailable as 503 through daemon API.

Runtime/system packages:

```text
x11-utils
xauth
libx11-dev
libxtst-dev
libxext-dev
```

## Input Backend

Implement real Linux input against qgui’s X display.

Preferred order:

1. XTest via `libXtst`/Rust binding for mouse and keyboard events.
2. No `xdotool` fallback in production. Input comes from the bundled Rust tool.

Required actions:

- `mouse_move`
- `mouse_click`
- `mouse_drag`
- `key_press`
- `key_type`
- `key_paste`
- `sequence`
- `open_app`
- `shell_exec`
- `aegis`
- `ctx`
- `pause`
- `resume`
- `kill_switch`

`open_app` and `shell_exec` should execute via:

```text
qgui run -- <command...>
```

Input evidence must distinguish:

- delivered to X display
- refused because qgui is unhealthy
- refused because action is unsupported
- refused because owner/session/safety/profile policy blocked it

## Clipboard

Implement qgui/Linux clipboard with the bundled Rust tool.

Options:

- X11 clipboard through a vendored Rust implementation.
- Rust X11 clipboard integration for production.

Do not advertise clipboard until read/write is tested under qgui.

## Image Build

Update `ops/oci/cloud-init-cua-node.yaml` or add a real image/rootfs recipe that installs:

```text
cua
qgui
cua-qgui-tool
kasmvncserver
kasmvncpasswd
xfce4-session
dbus-daemon
xrdb
xauth
xsetroot
x11-utils
libx11
libxtst
libxext
fonts
browser package
```

Boot target:

```text
systemd:
  qgui.service
  cua-daemon.service
```

`qgui.service`:

```text
ExecStart=/usr/local/bin/qgui up --bind 127.0.0.1 --port 6080 --res 1440x900
Restart=always
```

`cua-daemon.service`:

```text
Environment=CUA_COMPUTER_BACKEND=oracle-vm
ExecStart=/usr/local/bin/cua --profile cloud-node serve --addr 127.0.0.1:8765 --hud-mode headless
Restart=always
```

CUA should start after qgui and refuse readiness until qgui is usable.

## Control Plane

Do not publish KasmVNC directly to the internet.

Expose through an authenticated reverse proxy:

```text
/computer/<lease-id>/cua/*      -> CUA daemon HTTP
/computer/<lease-id>/desktop/*  -> KasmVNC browser endpoint
```

Required control-plane state:

- lease id
- provider
- instance id
- region/AD
- private/public address
- CUA bearer token
- qgui backend username/password if proxying KasmVNC basic auth
- owner session id, if pre-acquired
- expires at / TTL
- teardown policy

## Fleet Provider API

Keep provider lifecycle separate from computer control.

```rust
trait ComputerProvider {
    async fn allocate(request) -> ComputerAllocation;
    async fn status(instance_id) -> ComputerInstanceStatus;
    async fn release(instance_id) -> ComputerInstanceStatus;
}
```

Provider modules:

- `OciCliProvider`: current free-credit provider.
- `QuiltVmProvider`: future self-hosted provider.
- optional `AwsProvider`/`GenericProvider` later.

The agent should receive only a `ComputerBackend` once allocated. It should not know whether the machine came from Oracle Cloud Infrastructure, AWS, or Quilt VM.

## API Contract

The qgui-backed node must preserve the existing CUA protocol:

- `GET /status`
- `GET /session/status`
- `POST /session/acquire`
- `POST /capture/screenshot`
- `GET /observe/desktop`
- `GET /observe/displays`
- `GET /observe/cursor`
- `POST /input/dispatch`
- `POST /clipboard/read`
- `POST /clipboard/write`
- `POST /control/pause`
- `POST /control/resume`
- `POST /control/kill-switch`
- `GET /attestation/identity`
- `POST /attestation/challenge`
- `POST /attestation/sign`

The agent should not use KasmVNC directly for control. KasmVNC is for operator visibility and emergency manual takeover.

## Attestation

Cloud nodes still need machine identity and backend claims.

Add claims:

- provider: `oracle-vm` or `quilt-vm`
- runtime: `qgui+cua`
- qgui session hash
- qgui binary hash
- cua binary hash
- image/rootfs build id
- instance id
- region
- boot time
- capability manifest

Do not include:

- qgui password
- CUA bearer token
- private key material
- cloud API credentials

## Production Tests

Minimum before claiming real cloud computer use:

Local Linux/qgui unit and integration:

- qgui session JSON loads.
- qgui status JSON reports healthy components.
- qgui down kills KasmVNC and dbus.
- qgui run launches a visible test app.
- capture returns a nonblank frame.
- cursor position can be observed.
- mouse move changes cursor readback.
- click changes test-app state.
- key type enters text into test app.
- clipboard read/write works only when advertised.

Daemon protocol:

- `GET /status` reports qgui capabilities only after qgui health passes.
- `POST /input/dispatch` requires owner session.
- refused backend returns structured evidence.
- unavailable backend reports zero capabilities.
- unavailable observe/capture returns HTTP 503.
- remote CUA adapter can proxy to a qgui node.

Provider:

- Oracle VM launch produces a node that reaches qgui and CUA health.
- Oracle VM terminate tears down the node.
- Quilt VM provider can satisfy the same `ComputerProvider` contract.

Fozzy:

- strict deterministic scenario first
- host-backed run with real qgui where feasible
- record trace
- verify trace
- replay trace
- shrink trace
- CI trace check

Suggested live smoke:

```bash
cua cloud oci launch ...
ssh ubuntu@<ip> qgui status --json
ssh ubuntu@<ip> cua --profile cloud-node status --json
curl /capture/screenshot
curl /observe/desktop
curl /session/acquire
curl /input/dispatch mouse_move
curl /input/dispatch key_type into a test app
```

## Definition Of Done

This work is complete only when:

- A fresh Oracle VM node boots qgui and CUA automatically.
- The node reports `runtime=qgui+cua`.
- `GET /status` advertises real capture/input capabilities.
- Screenshot returns a real nonblank desktop frame.
- Observe reports real display/window/cursor state.
- Mouse and keyboard input mutate a visible test app.
- Remote CUA proxy can control the qgui node through `/input/dispatch`.
- All unavailable states report zero false capabilities.
- The same image can be launched by Oracle VM now and Quilt VM later with provider-only config changes.
