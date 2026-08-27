use async_trait::async_trait;
use cua_core::{
    DeliveryMode, Effect, Evidence, EvidenceKind, InputAction, InputRequest, InputResult,
    InputRoute, SCHEMA_VERSION,
};
use std::time::Instant;

#[async_trait]
pub trait InputBackend: Send + Sync {
    async fn execute(&self, request: InputRequest) -> InputResult;
    fn name(&self) -> &'static str;
}

#[derive(Debug, Default)]
pub struct RefusingInputBackend;

#[async_trait]
impl InputBackend for RefusingInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        let message = match request.action {
            InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => {
                "safety action accepted by local coordinator"
            }
            InputAction::Sequence { .. } => {
                "real desktop input is not enabled for this backend; refusing sequence"
            }
            _ => "real desktop input is not enabled for this backend; refusing instead of faking support",
        };
        let effect = match request.action {
            InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => Effect::Confirmed,
            _ => Effect::Refused,
        };
        InputResult {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: request.idempotency_key,
            effect,
            route: InputRoute::Unavailable,
            delivery_mode: DeliveryMode::Unknown,
            started_mono_ns: 0,
            ended_mono_ns: started.elapsed().as_nanos(),
            evidence: vec![Evidence {
                kind: EvidenceKind::Refusal,
                message: message.to_string(),
                frame_id: None,
            }],
        }
    }

    fn name(&self) -> &'static str {
        "refusing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cua_core::{MouseButton, SCHEMA_VERSION};
    use uuid::Uuid;

    #[tokio::test]
    async fn refuses_real_mouse_without_backend() {
        let backend = RefusingInputBackend;
        let result = backend
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: Uuid::new_v4(),
                deadline_mono_ns: None,
                action: InputAction::MouseClick {
                    x: 10,
                    y: 10,
                    button: MouseButton::Left,
                    count: 1,
                },
            })
            .await;
        assert_eq!(result.effect, Effect::Refused);
    }
}
