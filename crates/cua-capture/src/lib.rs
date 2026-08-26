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
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

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
    pub timings: CapturedFrameTimings,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CapturedFrameTimings {
    pub capture_ns: u64,
    pub encode_ns: u64,
}

#[derive(Debug, Clone)]
pub struct FrameLookup {
    pub frame: CapturedFrame,
    pub cache_hit: bool,
    pub wait_ns: u64,
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
        Ok(self.latest_or_capture_timed(request).await?.frame)
    }

    pub async fn latest_or_capture_timed(
        &self,
        request: CaptureRequest,
    ) -> anyhow::Result<FrameLookup> {
        let started = Instant::now();
        if !request.force_fresh {
            if let Some(frame) = self.latest.read().await.clone() {
                return Ok(FrameLookup {
                    frame,
                    cache_hit: true,
                    wait_ns: elapsed_ns(started),
                });
            }
        }
        let frame = match self.backend.capture_latest(request).await {
            Ok(frame) => frame,
            Err(error) => {
                if let Some(frame) = self.latest.read().await.clone() {
                    return Ok(FrameLookup {
                        frame,
                        cache_hit: true,
                        wait_ns: elapsed_ns(started),
                    });
                }
                return Err(error);
            }
        };
        *self.latest.write().await = Some(frame.clone());
        Ok(FrameLookup {
            frame,
            cache_hit: false,
            wait_ns: elapsed_ns(started),
        })
    }

    pub fn spawn_capture_lane(
        self: Arc<Self>,
        mut request: CaptureRequest,
        interval: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            request.force_fresh = true;
            loop {
                ticker.tick().await;
                if let Ok(frame) = self.backend.capture_latest(request.clone()).await {
                    *self.latest.write().await = Some(frame);
                }
            }
        })
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

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
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
        let capture_started = Instant::now();
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

        let encode_started = Instant::now();
        let bytes = encode_image(&image, request.encoding.clone())?;
        let encode_ns = elapsed_ns(encode_started);
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_len = bytes.len();
        let envelope = FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id,
            timestamp_mono_ns: self.started.elapsed().as_nanos(),
            timestamp_wall_ms: now_wall_ms(),
            display_id: "synthetic-primary".to_string(),
            display_width: width,
            display_height: height,
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
            timings: CapturedFrameTimings {
                capture_ns: elapsed_ns(capture_started),
                encode_ns,
            },
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
    use std::sync::atomic::{AtomicBool, Ordering};

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

    #[tokio::test]
    async fn capture_lane_populates_latest_slot() {
        let bus = Arc::new(FrameBus::new(Arc::new(SyntheticCaptureBackend::default())));
        let _lane = bus.clone().spawn_capture_lane(
            CaptureRequest {
                max_width: Some(320),
                encoding: FrameEncoding::Jpeg,
                force_fresh: true,
            },
            Duration::from_millis(5),
        );

        tokio::time::sleep(Duration::from_millis(30)).await;
        let envelope = bus.latest_envelope().await;
        assert!(envelope.is_some());
    }

    #[tokio::test]
    async fn fresh_capture_failure_uses_last_good_frame() {
        let backend = Arc::new(FlakyCaptureBackend {
            fail: AtomicBool::new(false),
        });
        let bus = FrameBus::new(backend.clone());
        let first = bus
            .latest_or_capture_timed(CaptureRequest {
                max_width: Some(320),
                encoding: FrameEncoding::Jpeg,
                force_fresh: true,
            })
            .await
            .unwrap();

        backend.fail.store(true, Ordering::SeqCst);
        let second = bus
            .latest_or_capture_timed(CaptureRequest {
                max_width: Some(320),
                encoding: FrameEncoding::Jpeg,
                force_fresh: true,
            })
            .await
            .unwrap();

        assert!(second.cache_hit);
        assert_eq!(
            first.frame.envelope.frame_id,
            second.frame.envelope.frame_id
        );
    }

    struct FlakyCaptureBackend {
        fail: AtomicBool,
    }

    #[async_trait]
    impl CaptureBackend for FlakyCaptureBackend {
        async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame> {
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("capture failed");
            }
            SyntheticCaptureBackend::default()
                .capture_latest(request)
                .await
        }

        async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>> {
            SyntheticCaptureBackend::default().displays().await
        }

        fn name(&self) -> &'static str {
            "flaky"
        }
    }
}
