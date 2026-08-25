use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use cua_core::{
    now_wall_ms, CursorState, DisplayInfo, FrameEncoding, FrameEnvelope, FramePayload, Rect,
    SCHEMA_VERSION,
};
use image::{codecs::jpeg::JpegEncoder, ImageBuffer, ImageEncoder, Rgba};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::RwLock;

#[async_trait]
pub trait CaptureBackend: Send + Sync {
    async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame>;
    async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>>;
    fn name(&self) -> &'static str;
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub max_width: Option<u32>,
    pub encoding: FrameEncoding,
    pub force_fresh: bool,
}

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub envelope: FrameEnvelope,
    pub bytes: Arc<Vec<u8>>,
}

impl CapturedFrame {
    pub fn as_payload(&self, include_bytes: bool) -> FramePayload {
        FramePayload {
            envelope: self.envelope.clone(),
            bytes_base64: include_bytes
                .then(|| base64::engine::general_purpose::STANDARD.encode(&*self.bytes)),
        }
    }
}

pub struct FrameBus {
    backend: Arc<dyn CaptureBackend>,
    latest: RwLock<Option<CapturedFrame>>,
    started: Instant,
}

impl FrameBus {
    pub fn new(backend: Arc<dyn CaptureBackend>) -> Self {
        Self {
            backend,
            latest: RwLock::new(None),
            started: Instant::now(),
        }
    }

    pub async fn latest_or_capture(
        &self,
        request: CaptureRequest,
    ) -> anyhow::Result<CapturedFrame> {
        if !request.force_fresh {
            if let Some(frame) = self.latest.read().await.clone() {
                return Ok(frame);
            }
        }
        let frame = self.backend.capture_latest(request).await?;
        *self.latest.write().await = Some(frame.clone());
        Ok(frame)
    }

    pub async fn latest_envelope(&self) -> Option<FrameEnvelope> {
        self.latest
            .read()
            .await
            .as_ref()
            .map(|f| f.envelope.clone())
    }

    pub async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>> {
        self.backend.displays().await
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn uptime_ns(&self) -> u128 {
        self.started.elapsed().as_nanos()
    }
}

#[derive(Debug)]
pub struct SyntheticCaptureBackend {
    started: Instant,
    width: u32,
    height: u32,
}

impl Default for SyntheticCaptureBackend {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            width: 1280,
            height: 720,
        }
    }
}

#[async_trait]
impl CaptureBackend for SyntheticCaptureBackend {
    async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame> {
        let width = request
            .max_width
            .unwrap_or(self.width)
            .min(self.width)
            .max(64);
        let height = ((self.height as f64) * (width as f64 / self.width as f64)).round() as u32;
        let frame_id = self.started.elapsed().as_millis() as u64;
        let mut image: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let checker = (((x / 48) + (y / 48) + (frame_id as u32 / 250)) % 2) as u8;
            let red = if checker == 0 { 32 } else { 210 };
            let green = ((x * 255 / width) as u8).saturating_add(20);
            let blue = ((y * 255 / height) as u8).saturating_add(10);
            *pixel = Rgba([red, green, blue, 255]);
        }

        let bytes = encode_image(&image, request.encoding.clone())?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_len = bytes.len();
        let envelope = FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id,
            timestamp_mono_ns: self.started.elapsed().as_nanos(),
            timestamp_wall_ms: now_wall_ms(),
            display_id: "synthetic-primary".to_string(),
            width,
            height,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: request.encoding,
            byte_len,
            sha256,
            cursor: CursorState {
                x: (width / 2) as f64,
                y: (height / 2) as f64,
                visible: true,
                included_in_frame: false,
            },
            damage_rects: vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
        };
        Ok(CapturedFrame {
            envelope,
            bytes: Arc::new(bytes),
        })
    }

    async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>> {
        Ok(vec![DisplayInfo {
            id: "synthetic-primary".to_string(),
            name: "Synthetic Primary Display".to_string(),
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
            scale_factor: 1.0,
            active: true,
        }])
    }

    fn name(&self) -> &'static str {
        "synthetic"
    }
}

pub fn encode_image(
    image: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    encoding: FrameEncoding,
) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        FrameEncoding::Png => {
            let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut out));
            encoder
                .write_image(
                    image,
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .context("encode png")?;
        }
        FrameEncoding::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(Cursor::new(&mut out), 82);
            encoder.encode_image(image).context("encode jpeg")?;
        }
        FrameEncoding::RawBgra => {
            out = image
                .pixels()
                .flat_map(|p| [p.0[2], p.0[1], p.0[0], p.0[3]])
                .collect();
        }
    }
    Ok(out)
}

pub fn monotonic_seed() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn synthetic_capture_returns_nonblank_png() {
        let backend = SyntheticCaptureBackend::default();
        let frame = backend
            .capture_latest(CaptureRequest {
                max_width: Some(320),
                encoding: FrameEncoding::Png,
                force_fresh: true,
            })
            .await
            .unwrap();
        assert_eq!(frame.envelope.width, 320);
        assert!(frame.envelope.byte_len > 1024);
        assert_eq!(frame.bytes.len(), frame.envelope.byte_len);
    }
}
