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
            speech_threshold: i16_from_env("CUA_VOICE_RECORD_THRESHOLD", 520, 80..=6_000),
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
    Ok(RecordedAudio {
        sample_rate,
        wav_bytes: encode_wav_mono(sample_rate, &samples)?,
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
        if let Some(sample) = frame.first() {
            let sample = (*sample).to_sample::<i16>();
            peak = peak.max(sample.saturating_abs());
            guard.push(sample);
        }
    }
    drop(guard);
    state
        .lock()
        .unwrap()
        .observe_peak(Instant::now(), peak, policy.speech_threshold);
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
        assert_eq!(policy.speech_threshold, 520);
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
        std::env::set_var(threshold_name, "10");
        assert_eq!(i16_from_env(threshold_name, 520, 80..=6_000), 520);
        std::env::set_var(threshold_name, "850");
        assert_eq!(i16_from_env(threshold_name, 520, 80..=6_000), 850);
        std::env::remove_var(threshold_name);
    }
}
