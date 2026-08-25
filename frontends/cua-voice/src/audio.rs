use anyhow::{bail, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RecordedAudio {
    pub sample_rate: u32,
    pub wav_bytes: Vec<u8>,
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

pub fn record_default_input(duration: Duration) -> anyhow::Result<RecordedAudio> {
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
    let err_fn = |err| eprintln!("cua voice input stream error: {err}");
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
    .context("build input stream")?;
    stream.play().context("start input stream")?;
    std::thread::sleep(duration);
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

fn push_interleaved<T>(data: &[T], channels: u16, samples: &Arc<Mutex<Vec<i16>>>)
where
    T: cpal::Sample,
    i16: cpal::FromSample<T>,
{
    let channels = usize::from(channels.max(1));
    let mut guard = samples.lock().unwrap();
    for frame in data.chunks(channels) {
        if let Some(sample) = frame.first() {
            guard.push((*sample).to_sample::<i16>());
        }
    }
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
}
