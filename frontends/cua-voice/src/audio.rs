use anyhow::{bail, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Cursor;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RecordedAudio {
    pub device_name: String,
    pub channels: u16,
    pub sample_format: String,
    pub sample_rate: u32,
    pub wav_bytes: Vec<u8>,
    pub duration: Duration,
    pub peak_amplitude: i16,
    pub rms_amplitude: f32,
}

#[derive(Debug, Copy, Clone)]
struct RecordingPolicy {
    max_duration: Duration,
}

impl RecordingPolicy {
    fn from_max_duration(max_duration: Duration) -> Self {
        Self { max_duration }
    }
}

pub fn encode_wav_mono(sample_rate: u32, samples: &[i16]) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::new(&mut bytes, spec).context("create wav writer")?;
        for sample in samples {
            writer.write_sample(*sample).context("write wav sample")?;
        }
        writer.finalize().context("finalize wav")?;
    }
    Ok(bytes.into_inner())
}

pub fn record_default_input(max_duration: Duration) -> anyhow::Result<RecordedAudio> {
    record_default_input_until(max_duration, Arc::new(AtomicBool::new(false)))
}

pub fn record_default_input_until(
    max_duration: Duration,
    stop_requested: Arc<AtomicBool>,
) -> anyhow::Result<RecordedAudio> {
    record_default_input_with_policy(
        RecordingPolicy::from_max_duration(max_duration),
        stop_requested,
    )
}

fn record_default_input_with_policy(
    policy: RecordingPolicy,
    stop_requested: Arc<AtomicBool>,
) -> anyhow::Result<RecordedAudio> {
    let host = cpal::default_host();
    let device = select_input_device(&host)?;
    let device_name = device.to_string();
    let config = device
        .default_input_config()
        .with_context(|| format!("read default input config {device_name}"))?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = format!("{:?}", config.sample_format());
    let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    let captured = samples.clone();
    let err_device = device_name.clone();
    let err_fn = move |err| eprintln!("cua voice input stream error on {err_device}: {err}");
    let stream_config: cpal::StreamConfig = config.clone().into();
    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config.clone(),
            move |data: &[i16], _| push_interleaved(data, channels, &captured),
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            stream_config.clone(),
            move |data: &[u16], _| push_interleaved(data, channels, &captured),
            err_fn,
            None,
        ),
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _| push_interleaved(data, channels, &captured),
            err_fn,
            None,
        ),
        sample_format => bail!("unsupported input sample format {sample_format:?}"),
    }
    .with_context(|| format!("build input stream {device_name}"))?;
    stream
        .play()
        .with_context(|| format!("start input stream {device_name}"))?;
    let started_at = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        if should_stop_recording(started_at, Instant::now(), policy, &stop_requested) {
            break;
        }
    }
    drop(stream);
    let samples = samples.lock().unwrap().clone();
    if samples.is_empty() {
        bail!("no samples captured from input device {device_name}");
    }
    let stats = audio_stats(sample_rate, &samples);
    let wav_samples = normalize_quiet_samples_for_stt(&samples, stats.peak_amplitude);
    Ok(RecordedAudio {
        device_name,
        channels,
        sample_format,
        sample_rate,
        wav_bytes: encode_wav_mono(sample_rate, &wav_samples)?,
        duration: stats.duration,
        peak_amplitude: stats.peak_amplitude,
        rms_amplitude: stats.rms_amplitude,
    })
}

fn select_input_device(host: &cpal::Host) -> anyhow::Result<cpal::Device> {
    let requested = std::env::var("CUA_VOICE_INPUT_DEVICE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(requested) = requested {
        return select_named_input_device(host, &requested);
    }
    select_named_input_device(host, "MacBook Pro Microphone").or_else(|_| {
        host.default_input_device()
            .context("no default input device available")
    })
}

fn select_named_input_device(host: &cpal::Host, requested: &str) -> anyhow::Result<cpal::Device> {
    let requested_lower = requested.to_lowercase();
    for device in host.input_devices().context("list input devices")? {
        let name = device.to_string();
        if name == requested || name.to_lowercase().contains(&requested_lower) {
            return Ok(device);
        }
    }
    bail!("input device not found: {requested}");
}

fn should_stop_recording(
    started_at: Instant,
    now: Instant,
    policy: RecordingPolicy,
    stop_requested: &AtomicBool,
) -> bool {
    stop_requested.load(Ordering::Acquire) || now.duration_since(started_at) >= policy.max_duration
}

fn push_interleaved<T>(data: &[T], channels: u16, samples: &Arc<Mutex<Vec<i16>>>)
where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    let channels = usize::from(channels.max(1));
    let mut guard = samples.lock().unwrap();
    for frame in data.chunks(channels) {
        if let Some((mixed, _frame_peak)) = mix_interleaved_frame(frame) {
            guard.push(mixed);
        }
    }
}

fn mix_interleaved_frame<T>(frame: &[T]) -> Option<(i16, i16)>
where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    if frame.is_empty() {
        return None;
    }
    let mut loudest = 0i16;
    let mut peak = 0i16;
    for sample in frame {
        let sample = (*sample).to_sample::<i16>();
        let magnitude = sample.saturating_abs();
        if magnitude > peak {
            peak = magnitude;
            loudest = sample;
        }
    }
    Some((loudest, peak))
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct AudioStats {
    duration: Duration,
    peak_amplitude: i16,
    rms_amplitude: f32,
}

fn audio_stats(sample_rate: u32, samples: &[i16]) -> AudioStats {
    if sample_rate == 0 || samples.is_empty() {
        return AudioStats {
            duration: Duration::ZERO,
            peak_amplitude: 0,
            rms_amplitude: 0.0,
        };
    }
    let mut peak = 0i16;
    let mut sum_squares = 0.0f64;
    for sample in samples {
        let magnitude = sample.saturating_abs();
        peak = peak.max(magnitude);
        let normalized = f64::from(*sample) / f64::from(i16::MAX);
        sum_squares += normalized * normalized;
    }
    let rms = (sum_squares / samples.len() as f64).sqrt() as f32;
    AudioStats {
        duration: Duration::from_secs_f64(samples.len() as f64 / f64::from(sample_rate)),
        peak_amplitude: peak,
        rms_amplitude: rms,
    }
}

fn normalize_quiet_samples_for_stt(samples: &[i16], peak: i16) -> Vec<i16> {
    const TARGET_PEAK: i32 = 3_200;
    const MAX_GAIN: i32 = 32;
    if peak <= 0 || i32::from(peak) >= TARGET_PEAK {
        return samples.to_vec();
    }
    let gain = (TARGET_PEAK / i32::from(peak)).clamp(1, MAX_GAIN);
    samples
        .iter()
        .map(|sample| {
            (i32::from(*sample) * gain).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_wav_header_and_samples() {
        let wav = encode_wav_mono(16_000, &[0, 100, -100]).unwrap();
        assert!(wav.starts_with(b"RIFF"));
        assert!(wav.windows(4).any(|chunk| chunk == b"WAVE"));
    }

    #[test]
    fn recorder_stops_at_max_duration_without_speech() {
        let start = Instant::now();
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(2),
        };
        let stop_requested = AtomicBool::new(false);

        assert!(!should_stop_recording(
            start,
            start + Duration::from_millis(1999),
            policy,
            &stop_requested
        ));
        assert!(should_stop_recording(
            start,
            start + Duration::from_secs(2),
            policy,
            &stop_requested
        ));
    }

    #[test]
    fn recorder_stops_immediately_when_requested() {
        let start = Instant::now();
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(5),
        };
        let stop_requested = AtomicBool::new(true);

        assert!(should_stop_recording(
            start,
            start + Duration::from_millis(20),
            policy,
            &stop_requested
        ));
    }

    #[test]
    fn recording_policy_defaults_are_latency_oriented() {
        let policy = RecordingPolicy::from_max_duration(Duration::from_secs(5));

        assert_eq!(policy.max_duration, Duration::from_secs(5));
    }

    #[test]
    fn audio_stats_reports_duration_peak_and_rms() {
        let stats = audio_stats(1_000, &[0, 1_000, -2_000, 0]);

        assert_eq!(stats.duration, Duration::from_millis(4));
        assert_eq!(stats.peak_amplitude, 2_000);
        assert!(stats.rms_amplitude > 0.03);
    }

    #[test]
    fn interleaved_input_uses_loudest_channel_for_mono_capture() {
        let frame = [0i16, 1_000i16];

        assert_eq!(mix_interleaved_frame(&frame), Some((1_000, 1_000)));
    }

    #[test]
    fn interleaved_input_does_not_cancel_inverted_channels() {
        let frame = [1_000i16, -1_000i16];

        assert_eq!(mix_interleaved_frame(&frame), Some((1_000, 1_000)));
    }

    #[test]
    fn interleaved_capture_keeps_loudest_channel() {
        let samples = Arc::new(Mutex::new(Vec::<i16>::new()));

        push_interleaved(&[0i16, 800i16, 0, 900], 2, &samples);

        assert_eq!(*samples.lock().unwrap(), vec![800, 900]);
    }

    #[test]
    fn quiet_samples_are_boosted_for_stt_without_changing_timing_stats() {
        let samples = [0i16, 50, -100, 25];

        let normalized = normalize_quiet_samples_for_stt(&samples, 100);

        assert_eq!(normalized, vec![0, 1_600, -3_200, 800]);
    }
}
