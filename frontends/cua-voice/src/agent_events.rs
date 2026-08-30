use crate::ui_state::VoiceUiEvent;
use cua_core::{IslandBackground, IslandScene, IslandTheme, UiMode};
use serde_json::Value;

pub fn agent_ui_event_from_daemon_event_advancing_cursor(
    event: &Value,
    last_sequence: &mut u64,
) -> Option<VoiceUiEvent> {
    let previous_sequence = *last_sequence;
    if let Some(sequence) = daemon_event_sequence(event) {
        *last_sequence = (*last_sequence).max(sequence);
    }
    agent_ui_event_from_daemon_event(event, previous_sequence).map(|(_, event)| event)
}

fn daemon_event_sequence(event: &Value) -> Option<u64> {
    event.get("sequence").and_then(|value| value.as_u64())
}

pub fn max_daemon_event_sequence(events: &[Value]) -> u64 {
    events
        .iter()
        .filter_map(daemon_event_sequence)
        .max()
        .unwrap_or(0)
}

pub fn agent_ui_event_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    agent_step_from_daemon_event(event, last_sequence)
        .or_else(|| agent_reply_from_daemon_event(event, last_sequence))
        .or_else(|| agent_mode_from_daemon_event(event, last_sequence))
        .or_else(|| agent_island_from_daemon_event(event, last_sequence))
        .or_else(|| agent_scene_from_daemon_event(event, last_sequence))
        .or_else(|| agent_visual_session_from_daemon_event(event, last_sequence))
        .or_else(|| agent_input_from_daemon_event(event, last_sequence))
}

pub fn agent_scene_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    match event.get("kind").and_then(|value| value.as_str())? {
        "ui_scene" => {
            let scene = event.get("data").and_then(|data| data.get("scene"))?;
            let scene = serde_json::from_value::<IslandScene>(scene.clone()).ok()?;
            scene.validate().ok()?;
            Some((sequence, VoiceUiEvent::SceneSet(scene)))
        }
        "ui_scene_reset" => Some((sequence, VoiceUiEvent::SceneReset)),
        "ui_scene_theme" => {
            let theme = event.get("data").and_then(|data| data.get("theme"))?;
            let theme = serde_json::from_value::<IslandTheme>(theme.clone()).ok()?;
            cua_core::validate_island_theme(&theme).ok()?;
            Some((sequence, VoiceUiEvent::SceneTheme(theme)))
        }
        "ui_scene_background" => {
            let background = event.get("data").and_then(|data| data.get("background"))?;
            let background = serde_json::from_value::<IslandBackground>(background.clone()).ok()?;
            cua_core::validate_island_background(&background).ok()?;
            Some((sequence, VoiceUiEvent::SceneBackground(background)))
        }
        _ => None,
    }
}

pub fn agent_island_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("ui_island") {
        return None;
    }
    let state = event
        .get("data")
        .and_then(|data| data.get("state"))
        .and_then(|value| value.as_str())?;
    let event = match state {
        "expanded" => VoiceUiEvent::SetExpanded(true),
        "collapsed" => VoiceUiEvent::SetExpanded(false),
        "minimized" => VoiceUiEvent::SetMinimized(true),
        "toggle" => VoiceUiEvent::ToggleExpanded,
        _ => return None,
    };
    Some((sequence, event))
}

pub fn agent_mode_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("ui_mode") {
        return None;
    }
    let mode = event
        .get("data")
        .and_then(|data| data.get("mode"))
        .and_then(|value| value.as_str())?;
    let mode = match mode {
        "headful" => UiMode::Headful,
        "headless" => UiMode::Headless,
        _ => return None,
    };
    let source = event
        .get("data")
        .and_then(|data| data.get("source"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    Some((sequence, VoiceUiEvent::UiMode { mode, source }))
}

pub fn agent_step_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("ui_step") {
        return None;
    }
    let data = event.get("data")?;
    let label = data.get("label").and_then(|value| value.as_str())?;
    let source = data
        .get("source")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let task = data
        .get("task")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tool = data
        .get("tool")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let step_index = data
        .get("step_index")
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok());
    let step_total = data
        .get("step_total")
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok());
    let ttl_ms = data.get("ttl_ms").and_then(|value| value.as_u64());
    if source.as_deref() == Some("voice") {
        return None;
    }
    Some((
        sequence,
        VoiceUiEvent::AgentStep {
            label: label.to_string(),
            source,
            task,
            tool,
            step_index,
            step_total,
            ttl_ms,
        },
    ))
}

pub fn agent_reply_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("ui_reply") {
        return None;
    }
    let data = event.get("data")?;
    let text = data.get("text").and_then(|value| value.as_str())?;
    let source = data.get("source").and_then(|value| value.as_str());
    if source == Some("voice") {
        return None;
    }
    Some((sequence, VoiceUiEvent::AutomationReply(text.to_string())))
}

pub fn agent_visual_session_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("visual_session_started") {
        return None;
    }
    let fps = event
        .get("data")
        .and_then(|data| data.get("fps"))
        .and_then(|value| value.as_u64())
        .unwrap_or(10);
    Some((
        sequence,
        VoiceUiEvent::AgentStep {
            label: format!("Streaming desktop frames at {fps} fps"),
            source: Some("remote".to_string()),
            task: Some("Computer control".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(1),
            step_total: Some(2),
            ttl_ms: Some(5_000),
        },
    ))
}

pub fn agent_input_from_daemon_event(
    event: &Value,
    last_sequence: u64,
) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    let kind = event.get("kind").and_then(|value| value.as_str())?;
    match kind {
        "input_started" => {
            let data = event.get("data");
            let label = data
                .and_then(|data| data.get("label"))
                .and_then(|value| value.as_str())
                .unwrap_or("remote action started")
                .to_string();
            Some((
                sequence,
                VoiceUiEvent::AutomationActivity {
                    label,
                    source: Some("Computer control".to_string()),
                    tool: Some("Unix socket".to_string()),
                },
            ))
        }
        "input_completed" => Some((
            sequence,
            VoiceUiEvent::AutomationActivity {
                label: "remote action accepted".to_string(),
                source: Some("Computer control".to_string()),
                tool: Some("Unix socket".to_string()),
            },
        )),
        "input_refused" => Some((
            sequence,
            VoiceUiEvent::Error("Remote action refused".to_string()),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_ui_step_event_maps_to_visible_agent_step() {
        let event = serde_json::json!({
            "sequence": 42,
            "kind": "ui_step",
            "data": {
                "label": "checking current focus",
                "source": "agent",
                "task": "debug auth",
                "tool": "browser",
                "step_index": 2,
                "step_total": 6,
                "ttl_ms": 1750
            }
        });

        let Some((
            sequence,
            VoiceUiEvent::AgentStep {
                label,
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            },
        )) = agent_step_from_daemon_event(&event, 41)
        else {
            panic!("expected agent step event");
        };

        assert_eq!(sequence, 42);
        assert_eq!(label, "checking current focus");
        assert_eq!(source.as_deref(), Some("agent"));
        assert_eq!(task.as_deref(), Some("debug auth"));
        assert_eq!(tool.as_deref(), Some("browser"));
        assert_eq!(step_index, Some(2));
        assert_eq!(step_total, Some(6));
        assert_eq!(ttl_ms, Some(1750));
        assert!(agent_step_from_daemon_event(&event, 42).is_none());
    }

    #[test]
    fn daemon_event_cursor_advances_across_ignored_events() {
        let ignored = serde_json::json!({
            "sequence": 1,
            "kind": "daemon_started",
            "data": {}
        });
        let step = serde_json::json!({
            "sequence": 2,
            "kind": "ui_step",
            "data": {
                "label": "typing proof text through cua",
                "task": "Live E2E",
                "tool": "CLI API",
                "step_index": 3,
                "step_total": 12
            }
        });
        let mut last_sequence = 0;

        assert!(
            agent_ui_event_from_daemon_event_advancing_cursor(&ignored, &mut last_sequence)
                .is_none()
        );
        assert_eq!(last_sequence, 1);

        let Some(VoiceUiEvent::AgentStep {
            label,
            task,
            tool,
            step_index,
            step_total,
            ..
        }) = agent_ui_event_from_daemon_event_advancing_cursor(&step, &mut last_sequence)
        else {
            panic!("expected visible agent step after ignored event");
        };

        assert_eq!(last_sequence, 2);
        assert_eq!(label, "typing proof text through cua");
        assert_eq!(task.as_deref(), Some("Live E2E"));
        assert_eq!(tool.as_deref(), Some("CLI API"));
        assert_eq!(step_index, Some(3));
        assert_eq!(step_total, Some(12));
    }

    #[test]
    fn max_daemon_event_sequence_ignores_malformed_events() {
        let events = vec![
            serde_json::json!({"sequence": 7, "kind": "ui_step"}),
            serde_json::json!({"kind": "ui_step"}),
            serde_json::json!({"sequence": 3, "kind": "ui_reply"}),
            serde_json::json!({"sequence": "bad", "kind": "ui_reply"}),
        ];

        assert_eq!(max_daemon_event_sequence(&events), 7);
    }

    #[test]
    fn daemon_ui_step_event_ignores_voice_telemetry_echoes() {
        let event = serde_json::json!({
            "sequence": 43,
            "kind": "ui_step",
            "data": {
                "label": "reply: done",
                "source": "voice"
            }
        });

        assert!(agent_step_from_daemon_event(&event, 42).is_none());
    }

    #[test]
    fn daemon_ui_reply_event_maps_to_visible_reply_flash() {
        let event = serde_json::json!({
            "sequence": 44,
            "kind": "ui_reply",
            "data": {
                "text": "Ready for the next step.",
                "source": "external agent",
                "ttl_ms": 1750
            }
        });

        let Some((sequence, VoiceUiEvent::AutomationReply(text))) =
            agent_reply_from_daemon_event(&event, 43)
        else {
            panic!("expected reply event");
        };

        assert_eq!(sequence, 44);
        assert_eq!(text, "Ready for the next step.");
        assert!(agent_reply_from_daemon_event(&event, 44).is_none());
    }

    #[test]
    fn daemon_ui_reply_event_ignores_voice_echoes() {
        let event = serde_json::json!({
            "sequence": 45,
            "kind": "ui_reply",
            "data": {
                "text": "internal voice reply",
                "source": "voice"
            }
        });

        assert!(agent_reply_from_daemon_event(&event, 44).is_none());
    }

    #[test]
    fn daemon_ui_mode_event_maps_to_live_visibility_mode() {
        let event = serde_json::json!({
            "sequence": 46,
            "kind": "ui_mode",
            "data": {
                "mode": "headless",
                "source": "cli"
            }
        });

        let Some((sequence, VoiceUiEvent::UiMode { mode, source })) =
            agent_mode_from_daemon_event(&event, 45)
        else {
            panic!("expected ui mode event");
        };

        assert_eq!(sequence, 46);
        assert_eq!(mode, UiMode::Headless);
        assert_eq!(source.as_deref(), Some("cli"));
        assert!(agent_mode_from_daemon_event(&event, 46).is_none());
    }

    #[test]
    fn daemon_ui_island_event_maps_to_expansion_command() {
        let expanded = serde_json::json!({
            "sequence": 47,
            "kind": "ui_island",
            "data": {
                "state": "expanded",
                "source": "automation"
            }
        });
        let collapsed = serde_json::json!({
            "sequence": 48,
            "kind": "ui_island",
            "data": {
                "state": "collapsed"
            }
        });
        let toggle = serde_json::json!({
            "sequence": 49,
            "kind": "ui_island",
            "data": {
                "state": "toggle"
            }
        });
        let minimized = serde_json::json!({
            "sequence": 50,
            "kind": "ui_island",
            "data": {
                "state": "minimized"
            }
        });

        let Some((sequence, VoiceUiEvent::SetExpanded(true))) =
            agent_island_from_daemon_event(&expanded, 46)
        else {
            panic!("expected expanded island event");
        };
        assert_eq!(sequence, 47);

        let Some((sequence, VoiceUiEvent::SetExpanded(false))) =
            agent_island_from_daemon_event(&collapsed, 47)
        else {
            panic!("expected collapsed island event");
        };
        assert_eq!(sequence, 48);

        let Some((sequence, VoiceUiEvent::ToggleExpanded)) =
            agent_island_from_daemon_event(&toggle, 48)
        else {
            panic!("expected toggle island event");
        };
        assert_eq!(sequence, 49);
        let Some((sequence, VoiceUiEvent::SetMinimized(true))) =
            agent_island_from_daemon_event(&minimized, 49)
        else {
            panic!("expected minimized island event");
        };
        assert_eq!(sequence, 50);
        assert!(agent_island_from_daemon_event(&toggle, 49).is_none());
    }

    #[test]
    fn daemon_ui_scene_event_maps_to_programmed_scene() {
        let event = serde_json::json!({
            "sequence": 50,
            "kind": "ui_scene",
            "data": {
                "scene": {
                    "schema_version": "cua.island.v1",
                    "layout": "compact",
                    "mode": "headful",
                    "background": {"kind": "solid", "color": "#000000", "opacity": 92},
                    "regions": {
                        "left": {
                            "items": [
                                {"kind": "label", "id": "orb", "text": "orb"},
                                {"kind": "label", "id": "input", "text": "Automation"}
                            ]
                        },
                        "center": {
                            "items": [
                                {"kind": "marquee", "id": "status", "text": "Programmable scene"}
                            ]
                        },
                        "right": {
                            "items": [
                                {"kind": "chip", "id": "transport", "text": "Socket"},
                                {"kind": "chip", "id": "target", "text": "macOS"},
                                {"kind": "dot_chase", "id": "activity", "active": true, "palette": "blue_neon", "count": 6, "speed": 8}
                            ]
                        }
                    },
                    "ambient": [],
                    "actors": [],
                    "theme": null
                }
            }
        });

        let Some((sequence, VoiceUiEvent::SceneSet(scene))) =
            agent_scene_from_daemon_event(&event, 49)
        else {
            panic!("expected scene event");
        };

        assert_eq!(sequence, 50);
        assert_eq!(scene.layout, cua_core::IslandLayout::Compact);
        assert!(agent_scene_from_daemon_event(&event, 50).is_none());
    }

    #[test]
    fn daemon_ui_scene_background_event_maps_to_programmed_background() {
        let event = serde_json::json!({
            "sequence": 51,
            "kind": "ui_scene_background",
            "data": {
                "background": {
                    "kind": "neon_sweep",
                    "base_color": "#000000",
                    "sweep_color": "#1e9bff",
                    "opacity": 88,
                    "duration_ms": 1200
                }
            }
        });

        let Some((sequence, VoiceUiEvent::SceneBackground(background))) =
            agent_scene_from_daemon_event(&event, 50)
        else {
            panic!("expected background event");
        };

        assert_eq!(sequence, 51);
        assert!(matches!(
            background,
            cua_core::IslandBackground::NeonSweep {
                duration_ms: 1200,
                ..
            }
        ));
        assert!(agent_scene_from_daemon_event(&event, 51).is_none());
    }

    #[test]
    fn daemon_ui_scene_reset_event_maps_to_scene_reset() {
        let event = serde_json::json!({
            "sequence": 51,
            "kind": "ui_scene_reset",
            "data": {"source": "test"}
        });

        let Some((sequence, VoiceUiEvent::SceneReset)) = agent_scene_from_daemon_event(&event, 50)
        else {
            panic!("expected scene reset");
        };

        assert_eq!(sequence, 51);
        assert!(agent_scene_from_daemon_event(&event, 51).is_none());
    }

    #[test]
    fn daemon_ui_scene_theme_event_maps_to_theme() {
        let event = serde_json::json!({
            "sequence": 52,
            "kind": "ui_scene_theme",
            "data": {
                "theme": {
                    "name": "default",
                    "tokens": {"blue": "#1e9bff"}
                }
            }
        });

        let Some((sequence, VoiceUiEvent::SceneTheme(theme))) =
            agent_scene_from_daemon_event(&event, 51)
        else {
            panic!("expected scene theme");
        };

        assert_eq!(sequence, 52);
        assert_eq!(theme.name, "default");
        assert_eq!(theme.tokens["blue"], "#1e9bff");
    }

    #[test]
    fn daemon_input_completed_event_maps_to_automation_activity() {
        let event = serde_json::json!({
            "sequence": 46,
            "kind": "input_completed",
            "data": {
                "effect": "confirmed",
                "route": "accessibility",
                "evidence_kind": "cursor_readback"
            }
        });

        let Some((
            sequence,
            VoiceUiEvent::AutomationActivity {
                label,
                source,
                tool,
            },
        )) = agent_input_from_daemon_event(&event, 45)
        else {
            panic!("expected automation activity");
        };
        assert_eq!(sequence, 46);
        assert_eq!(label, "remote action accepted");
        assert_eq!(source.as_deref(), Some("Computer control"));
        assert_eq!(tool.as_deref(), Some("Unix socket"));
        assert!(agent_input_from_daemon_event(&event, 46).is_none());
    }

    #[test]
    fn daemon_input_started_event_maps_to_immediate_automation_activity() {
        let event = serde_json::json!({
            "sequence": 74,
            "kind": "input_started",
            "data": {
                "label": "mouse move to 30,40",
                "source": "automation",
                "tool": "Unix socket"
            }
        });

        let Some((
            sequence,
            VoiceUiEvent::AutomationActivity {
                label,
                source,
                tool,
            },
        )) = agent_input_from_daemon_event(&event, 73)
        else {
            panic!("expected automation activity");
        };

        assert_eq!(sequence, 74);
        assert_eq!(label, "mouse move to 30,40");
        assert_eq!(source.as_deref(), Some("Computer control"));
        assert_eq!(tool.as_deref(), Some("Unix socket"));
    }

    #[test]
    fn visual_session_event_maps_to_visible_stream_activity() {
        let event = serde_json::json!({
            "sequence": 47,
            "kind": "visual_session_started",
            "data": {
                "fps": 12,
                "max_width": 1280,
                "include_bytes": false
            }
        });

        let Some((
            sequence,
            VoiceUiEvent::AgentStep {
                label,
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            },
        )) = agent_visual_session_from_daemon_event(&event, 46)
        else {
            panic!("expected visual stream activity");
        };

        assert_eq!(sequence, 47);
        assert_eq!(label, "Streaming desktop frames at 12 fps");
        assert_eq!(source.as_deref(), Some("remote"));
        assert_eq!(task.as_deref(), Some("Computer control"));
        assert_eq!(tool.as_deref(), Some("Unix socket"));
        assert_eq!(step_index, Some(1));
        assert_eq!(step_total, Some(2));
        assert_eq!(ttl_ms, Some(5_000));
    }
}
