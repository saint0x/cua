# UI Refactor

## Goal

Move the HUD from hardcoded view state into a small, strict, programmable island scene protocol while keeping the current UI visually identical as the baseline.

The first successful version of this refactor must render the existing compact and expanded HUD 1:1 through the new protocol. The protocol is allowed to make the HUD more expressive later, but the refactor itself must not change the current visual design, sizing, alignment, motion feel, labels, dot behavior, hover controls, or top-of-screen placement.

## Non-Negotiables

- Preserve the current UI exactly as the baseline scene.
- Keep the current Rust/GPUI architecture; improve the boundaries, do not replace the app with a webview or arbitrary plugin runtime.
- No backwards compatibility layer. Existing HUD state should be cut over cleanly to the new scene model.
- No arbitrary executable UI code inside the island.
- All programmable UI must be declarative, validated, bounded, and safe to render.
- The island must stay fast, native, click-clean, draggable, and magnetically attached as it is now.
- Region content must not depend on actor position.
- Actors may react to region content, but actors must not resize, reflow, or destabilize the production HUD.

## Current Baseline

The existing HUD already has a strong separation:

- `frontends/cua-voice/src/ui_state.rs`
  - Owns semantic runtime state: phase, input mode, task, step, tool, transcript, response, timers.
- `frontends/cua-voice/src/hud.rs`
  - Converts semantic state into display labels, rows, chips, metrics, and compact/expanded dimensions.
- `frontends/cua-voice/src/main.rs`
  - Renders the GPUI island, stoplights, compact bar, expanded body, marquee, dots, hover state, drag behavior, and window bounds.
- `crates/cua-daemon/src/lib.rs`
  - Exposes programmable UI events through HTTP, Unix socket, and CLI: `ui.step`, `ui.reply`, `ui.mode`, and island state changes.

The refactor should preserve that shape, but insert one new explicit boundary:

```text
Semantic HUD State -> IslandScene -> GPUI Renderer
```

## Core Design

The island should be one programmable scene with named regions, not three separate mini-apps.

```text
IslandScene
  Canvas
    Regions
      left
      center
      right
    Layers
      background
      content
      ambient
      actor
      foreground
```

The left, center, and right sections are stable layout regions. Programmable elements live inside those regions when they are part of normal HUD content. Cross-region animated elements live in the shared canvas coordinate space.

This allows:

- The left region to keep mode/source identity such as `Voice control` or `Automation`.
- The center region to keep status, step, transcript, or response text.
- The right region to keep transport, target, tool chips, and activity dots.
- Ambient elements to render behind or beside content without disturbing layout.
- Actor elements, such as a small digital pet, to move across the whole island and interact visually with region content.

## Protocol Shape

The first protocol should be intentionally small.

```json
{
  "schema_version": "cua.island.v1",
  "layout": "compact",
  "mode": "headful",
  "regions": {
    "left": {
      "items": [
        {
          "id": "input",
          "kind": "label",
          "text": "Voice control"
        }
      ]
    },
    "center": {
      "items": [
        {
          "id": "status",
          "kind": "marquee",
          "text": "Ready"
        }
      ]
    },
    "right": {
      "items": [
        {
          "id": "transport",
          "kind": "chip",
          "text": "Socket"
        },
        {
          "id": "target",
          "kind": "chip",
          "text": "macOS"
        },
        {
          "id": "activity",
          "kind": "dot_chase",
          "active": false,
          "palette": "blue_neon"
        }
      ]
    }
  },
  "actors": []
}
```

Expanded mode uses the same scene root, but with expanded layout regions:

```json
{
  "schema_version": "cua.island.v1",
  "layout": "expanded",
  "regions": {
    "header_left": [],
    "header_center": [],
    "header_right": [],
    "body": [],
    "footer": []
  }
}
```

## Scene Items

Initial item types:

- `label`
  - Single-line text.
  - Used for input mode, phase, compact labels, and table labels.
- `marquee`
  - Single-line text that scrolls only when it overflows.
  - Used for center status and collapsed agent responses.
- `chip`
  - Compact status pill.
  - Used for transport, target, tool, model, and surface labels.
- `step_counter`
  - Displays `x/y` with small, crisp hierarchy.
  - Must support any valid total, not just `0/4`.
- `dot_chase`
  - Six fixed positions with cyclic head/trailing opacity.
  - Default palette is one neon blue ramp, not competing colors.
- `row`
  - Expanded HUD row primitive for task/action/tool/status details.
- `divider`
  - Thin structural separator.
- `spacer`
  - Fixed or flexible layout spacing.

Later item types:

- `meter`
  - Small activity/progress/signal indicator.
- `icon`
  - Small bounded symbol from a known native set.
- `sprite`
  - Bounded image or vector actor.
- `particle`
  - Tiny ambient accent with strict count and lifecycle limits.

## Actors

Actors are optional scene elements rendered in the shared canvas, above content or between ambient/content depending on layer.

They are how a future digital pet or expressive ambient companion should work.

```json
{
  "id": "pet",
  "kind": "sprite",
  "layer": "actor",
  "anchor": "canvas",
  "x": 92,
  "y": 21,
  "bounds": "island",
  "motion": {
    "kind": "walk_to",
    "target": {
      "region": "right",
      "item": "activity"
    },
    "duration_ms": 900
  },
  "interactions": [
    {
      "on": "listening",
      "motion": "attend_center"
    },
    {
      "on": "reply",
      "motion": "idle_near_center"
    }
  ]
}
```

Actor rules:

- Actors use the island canvas coordinate system.
- Actors may reference region items as anchors.
- Actors cannot change region layout.
- Actors cannot intercept clicks unless explicitly marked interactive.
- Actor hitboxes must be smaller than their visual bounds and must never block the main bar controls.
- Actor motion must be deterministic and time-bounded.
- Actor count must be capped.

## Layout Contract

The current compact island is the baseline contract:

- Top attached.
- Black background.
- Same compact width, height, radius, and alignment.
- Same small crisp off-white typography.
- Same left input label position.
- Same center status/marquee behavior.
- Same right chips and blue activity dots.
- Same hover-only stoplights.
- Same click-through and drag behavior.

The current expanded island is also baseline:

- Same shell dimensions.
- Same compact header.
- Same table-like hierarchy.
- Same small labels and blue index tags.
- Same response persistence rules.
- Same collapse and minimize behavior.

The new scene renderer must reproduce these through declarative primitives before new customization is exposed.

## Rendering Pipeline

Target pipeline:

```text
VoiceUiEvent / daemon event
  -> HudSnapshot
  -> IslandScene::from_snapshot(snapshot)
  -> validate_scene(scene)
  -> GPUI render(scene)
```

The existing state model should remain semantic. The scene model should be render-oriented.

Do not push low-level pixel positions into the agent loop for normal usage. Agents should usually send semantic events like `ui.step`, `ui.reply`, `ui.mode`, or later `ui.scene.patch`. The runtime turns those into safe scene updates.

## Programmability Surface

Keep the existing high-level UI API:

- `ui.step`
- `ui.reply`
- `ui.mode`
- `ui.island`

Add a scene-level API:

- `ui.scene.set`
- `ui.scene.patch`
- `ui.scene.reset`
- `ui.scene.theme`

The scene API should be available over the same control layers:

- Unix socket
- HTTP API
- CLI
- Runebook
- SDK wrappers

No separate UI server should be introduced.

## Validation

Every incoming scene or scene patch must be validated before it reaches GPUI.

Validate:

- Schema version.
- Known item kinds only.
- Known region names only.
- Required fields by item kind.
- Text length limits.
- Item count limits.
- Actor count limits.
- Pixel bounds.
- Animation duration bounds.
- Palette names or explicit color limits.
- No negative sizes.
- No unbounded timers.
- No interactive element outside island bounds.
- No item may force window resizing outside approved compact/expanded metrics.

Invalid scenes should fail loudly with typed protocol errors.

## Default Scene

The current HUD should become the built-in default scene.

Default compact regions:

- `left`
  - orb
  - input label
- `center`
  - phase/status/step/reply marquee
- `right`
  - transport chip
  - target chip
  - dot chase

Default expanded regions:

- `header`
  - same compact top row
- `task`
  - task name and step counter
- `response`
  - response body
- `details_left`
  - action, phase, state
- `details_right`
  - recent tool rows
- `footer`
  - elapsed, model, transport

## Motion

Motion should be declarative and preset-based.

Initial presets:

- `none`
- `fade`
- `marquee`
- `dot_chase`
- `pulse`
- `spring_expand`
- `spring_collapse`
- `slide_to`
- `walk_to`

The six-dot activity indicator should be represented as a `dot_chase` primitive:

```text
distance = (head_index - dot_index + dot_count) % dot_count
0 -> opacity 1.00
1 -> opacity 0.60
2 -> opacity 0.30
else -> opacity 0.10
```

The palette should be one neon blue ramp by default.

## Theming

Themes should be small token sets, not arbitrary CSS.

```json
{
  "name": "default",
  "tokens": {
    "background": "#000000",
    "text": "#e8e8ec",
    "muted": "#8b8b95",
    "blue": "#1e9bff",
    "chip_background": "#1f1f22",
    "divider": "#1b1b1f"
  }
}
```

Agents can request approved theme tokens, but the app should preserve readability and contrast.

## Refactor Plan

1. Add `IslandScene` data types.
   - Put protocol structs in a shared crate if daemon, CLI, SDKs, and HUD all need them.
   - Keep item kinds strict enums.
   - Derive serde types.

2. Add `IslandScene::from_snapshot`.
   - Map the existing `HudSnapshot` into the default compact/expanded scene.
   - This must render identically to the current UI.

3. Add scene validation.
   - Unit test every item kind.
   - Unit test invalid bounds, invalid region, too much text, too many actors.

4. Cut GPUI rendering over to `IslandScene`.
   - Preserve exact current metrics.
   - Preserve hover controls, click-through behavior, drag behavior, and magnetic top placement.
   - Do not expose customization until the baseline renders 1:1.

5. Add protocol methods.
   - `ui.scene.set`
   - `ui.scene.patch`
   - `ui.scene.reset`
   - `ui.scene.theme`

6. Wire scene events through daemon event lane.
   - HTTP, Unix socket, CLI, Runebook, and SDKs should all use the same core contract.

7. Add actor support.
   - Start with noninteractive bounded sprites.
   - Add anchor references to region items.
   - Add deterministic motion presets.

8. Add programmable ambient patterns.
   - Keep `dot_chase` as the default.
   - Add named patterns only after the baseline is proven.

9. Add production proofs.
   - Visual proof that default compact matches baseline geometry.
   - Visual proof that expanded mode matches baseline geometry.
   - Protocol proof that scene set/patch/reset works over Unix, HTTP, CLI, Runebook, and SDK.
   - Regression proof that actors cannot reflow content or block clicks.

## Done Means

- The current UI renders through `IslandScene`.
- Compact HUD is visually unchanged.
- Expanded HUD is visually unchanged.
- All labels, chips, dots, marquee behavior, response persistence, hover controls, and drag/minimize behavior are unchanged.
- `ui.step`, `ui.reply`, and `ui.mode` still work.
- New scene protocol works through Unix socket, HTTP, CLI, Runebook, and SDKs.
- Invalid scenes produce typed errors.
- Actors can move across left, center, and right regions without shifting layout.
- Activity dots remain the default neon blue cyclic chase.
- Host visual proof passes.
- Full workspace tests pass.
- Release script installs and relaunches the app successfully.

