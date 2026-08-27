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
    pub sample_rate: u32,
    pub wav_bytes: Vec<u8>,
    pub duration: Duration,
    pub peak_amplitude: i16,
    pub rms_amplitude: f32,
}

#[derive(Debug, Copy, Clone)]
struct RecordingPolicy {
    max_duration: Duration,
    min_duration: Duration,
    silence_duration: Duration,
    speech_threshold: i16,
}

impl RecordingPolicy {
    fn from_max_duration(max_duration: Duration) -> Self {
        Self {
            max_duration,
            min_duration: duration_from_env("CUA_VOICE_RECORD_MIN_MS", 350, 100..=2_000),
            silence_duration: duration_from_env("CUA_VOICE_RECORD_SILENCE_MS", 420, 120..=2_000),
            speech_threshold: i16_from_env("CUA_VOICE_RECORD_THRESHOLD", 48, 8..=6_000),
        }
    }
}

fn duration_from_env(
    name: &str,
    default_ms: u64,
    valid_range: std::ops::RangeInclusive<u64>,
) -> Duration {
    Duration::from_millis(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| valid_range.contains(value))
            .unwrap_or(default_ms),
    )
}

fn i16_from_env(name: &str, default_value: i16, valid_range: std::ops::RangeInclusive<i16>) -> i16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i16>().ok())
        .filter(|value| valid_range.contains(value))
        .unwrap_or(default_value)
}

#[derive(Debug, Clone)]
struct RecordingState {
    started_at: Instant,
    last_voice_at: Option<Instant>,
    heard_voice: bool,
}

impl RecordingState {
    fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            last_voice_at: None,
            heard_voice: false,
        }
    }

    fn observe_peak(&mut self, now: Instant, peak: i16, threshold: i16) {
        if peak >= threshold {
            self.heard_voice = true;
            self.last_voice_at = Some(now);
        }
    }

    fn should_stop(&self, now: Instant, policy: RecordingPolicy) -> bool {
        let elapsed = now.duration_since(self.started_at);
        if elapsed >= policy.max_duration {
            return true;
        }
        if elapsed < policy.min_duration || !self.heard_voice {
            return false;
        }
        self.last_voice_at
            .map(|last_voice_at| now.duration_since(last_voice_at) >= policy.silence_duration)
            .unwrap_or(false)
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
    let device = host
        .default_input_device()
        .context("no default input device available")?;
    let config = device
        .default_input_config()
        .context("read default input config")?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    let captured = samples.clone();
    let state = Arc::new(Mutex::new(RecordingState::new(Instant::now())));
    let i16_state = state.clone();
    let u16_state = state.clone();
    let f32_state = state.clone();
    let err_fn = |err| eprintln!("cua voice input stream error: {err}");
    let stream_config: cpal::StreamConfig = config.clone().into();
    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config.clone(),
            move |data: &[i16], _| push_interleaved(data, channels, &captured, &i16_state, policy),
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            stream_config.clone(),
            move |data: &[u16], _| push_interleaved(data, channels, &captured, &u16_state, policy),
            err_fn,
            None,
        ),
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _| push_interleaved(data, channels, &captured, &f32_state, policy),
            err_fn,
            None,
        ),
        sample_format => bail!("unsupported input sample format {sample_format:?}"),
    }
    .context("build input stream")?;
    stream.play().context("start input stream")?;
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let should_stop = {
            let state = state.lock().unwrap();
            should_stop_recording(&state, Instant::now(), policy, &stop_requested)
        };
        if should_stop {
            break;
        }
    }
    drop(stream);
    let samples = samples.lock().unwrap().clone();
    if samples.is_empty() {
        bail!("no samples captured from input device");
    }
    let stats = audio_stats(sample_rate, &samples);
    let wav_samples = normalize_quiet_samples_for_stt(&samples, stats.peak_amplitude);
    Ok(RecordedAudio {
        sample_rate,
        wav_bytes: encode_wav_mono(sample_rate, &wav_samples)?,
        duration: stats.duration,
        peak_amplitude: stats.peak_amplitude,
        rms_amplitude: stats.rms_amplitude,
    })
}

fn should_stop_recording(
    state: &RecordingState,
    now: Instant,
    policy: RecordingPolicy,
    stop_requested: &AtomicBool,
) -> bool {
    stop_requested.load(Ordering::Acquire) || state.should_stop(now, policy)
}

fn push_interleaved<T>(
    data: &[T],
    channels: u16,
    samples: &Arc<Mutex<Vec<i16>>>,
    state: &Arc<Mutex<RecordingState>>,
    policy: RecordingPolicy,
) where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    let channels = usize::from(channels.max(1));
    let mut guard = samples.lock().unwrap();
    let mut peak = 0i16;
    for frame in data.chunks(channels) {
        if let Some((mixed, frame_peak)) = mix_interleaved_frame(frame) {
            peak = peak.max(frame_peak);
            guard.push(mixed);
        }
    }
    drop(guard);
    state
        .lock()
        .unwrap()
        .observe_peak(Instant::now(), peak, policy.speech_threshold);
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
    fn recorder_waits_for_speech_before_silence_stop() {
        let start = Instant::now();
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(5),
            min_duration: Duration::from_millis(200),
            silence_duration: Duration::from_millis(300),
            speech_threshold: 100,
        };
        let mut state = RecordingState::new(start);

        assert!(!state.should_stop(start + Duration::from_secs(1), policy));
        state.observe_peak(
            start + Duration::from_millis(250),
            120,
            policy.speech_threshold,
        );
        assert!(!state.should_stop(start + Duration::from_millis(400), policy));
        assert!(state.should_stop(start + Duration::from_millis(560), policy));
    }

    #[test]
    fn recorder_stops_at_max_duration_without_speech() {
        let start = Instant::now();
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(2),
            min_duration: Duration::from_millis(200),
            silence_duration: Duration::from_millis(300),
            speech_threshold: 100,
        };
        let state = RecordingState::new(start);

        assert!(!state.should_stop(start + Duration::from_millis(1999), policy));
        assert!(state.should_stop(start + Duration::from_secs(2), policy));
    }

    #[test]
    fn recorder_stops_immediately_when_requested() {
        let start = Instant::now();
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(5),
            min_duration: Duration::from_millis(200),
            silence_duration: Duration::from_millis(300),
            speech_threshold: 100,
        };
        let state = RecordingState::new(start);
        let stop_requested = AtomicBool::new(true);

        assert!(should_stop_recording(
            &state,
            start + Duration::from_millis(20),
            policy,
            &stop_requested
        ));
    }

    #[test]
    fn recording_policy_defaults_are_latency_oriented() {
        let policy = RecordingPolicy::from_max_duration(Duration::from_secs(5));

        assert_eq!(policy.min_duration, Duration::from_millis(350));
        assert_eq!(policy.silence_duration, Duration::from_millis(420));
        assert_eq!(policy.speech_threshold, 48);
    }

    #[test]
    fn recording_policy_env_bounds_ignore_invalid_values() {
        let name = "__CUA_VOICE_TEST_DURATION";
        std::env::set_var(name, "5");
        assert_eq!(
            duration_from_env(name, 250, 100..=1_000),
            Duration::from_millis(250)
        );
        std::env::set_var(name, "900");
        assert_eq!(
            duration_from_env(name, 250, 100..=1_000),
            Duration::from_millis(900)
        );
        std::env::remove_var(name);

        let threshold_name = "__CUA_VOICE_TEST_THRESHOLD";
        std::env::set_var(threshold_name, "4");
        assert_eq!(i16_from_env(threshold_name, 48, 8..=6_000), 48);
        std::env::set_var(threshold_name, "850");
        assert_eq!(i16_from_env(threshold_name, 48, 8..=6_000), 850);
        std::env::remove_var(threshold_name);
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
    fn interleaved_peak_detects_speech_outside_first_channel() {
        let policy = RecordingPolicy {
            max_duration: Duration::from_secs(1),
            min_duration: Duration::from_millis(100),
            silence_duration: Duration::from_millis(120),
            speech_threshold: 500,
        };
        let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
        let state = Arc::new(Mutex::new(RecordingState::new(Instant::now())));

        push_interleaved(&[0i16, 800i16, 0, 900], 2, &samples, &state, policy);

        assert_eq!(*samples.lock().unwrap(), vec![800, 900]);
        assert!(state.lock().unwrap().heard_voice);
    }

    #[test]
    fn quiet_samples_are_boosted_for_stt_without_changing_timing_stats() {
        let samples = [0i16, 50, -100, 25];

        let normalized = normalize_quiet_samples_for_stt(&samples, 100);

        assert_eq!(normalized, vec![0, 1_600, -3_200, 800]);
    }
}
