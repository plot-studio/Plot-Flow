//! Audio capture for meeting mode: the room microphone and the computer's
//! own playback (WASAPI loopback of the default output device).
//!
//! Each opened source is a cpal input stream whose real-time callback only
//! down-mixes to mono and pushes into a wait-free SPSC ring (`rtrb`), exactly
//! like `audio_toolkit::audio::recorder`. Resampling, VAD and everything else
//! happen on the per-source processing thread in `session.rs`.
//!
//! cpal streams are not guaranteed to be `Send`, so a [`CaptureStream`] must
//! be created and dropped on the same thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use super::types::MeetingSource;

/// Seconds of audio the ring can hold before the callback starts dropping.
const RING_SECONDS: usize = 3;

/// Flags shared between the real-time callback and the processing thread.
#[derive(Default)]
pub struct CaptureFlags {
    /// Set by cpal's error callback (device unplugged, invalidated, ...).
    pub stream_error: AtomicBool,
    /// Mono samples dropped because the ring was full.
    pub overrun_samples: AtomicU64,
}

/// A running capture stream plus the consumer side of its sample ring.
pub struct CaptureStream {
    /// Keeps the cpal stream alive; dropping it stops capture.
    _stream: cpal::Stream,
    /// Mono f32 samples at [`CaptureStream::sample_rate`].
    pub consumer: Consumer<f32>,
    /// Native sample rate of the device.
    pub sample_rate: u32,
    /// Human-readable device name (for logs and error messages).
    pub device_name: String,
    /// Error/overrun flags written by the callbacks.
    pub flags: Arc<CaptureFlags>,
}

impl CaptureStream {
    /// Pass every currently buffered mono sample to `process` (at most two
    /// slices because the ring wraps). Returns the number of samples drained.
    pub fn drain(&mut self, mut process: impl FnMut(&[f32])) -> usize {
        let available = self.consumer.slots();
        if available == 0 {
            return 0;
        }
        match self.consumer.read_chunk(available) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                if !first.is_empty() {
                    process(first);
                }
                if !second.is_empty() {
                    process(second);
                }
                chunk.commit_all();
                available
            }
            Err(_) => 0,
        }
    }

    /// True once cpal reported that the stream can no longer deliver audio.
    pub fn has_failed(&self) -> bool {
        self.flags.stream_error.load(Ordering::Acquire)
    }
}

/// Open the capture stream for `source`.
///
/// * `Microphone`: the microphone selected in settings (`selected_microphone`),
///   falling back to the system default input when it is missing — the same
///   resolution `managers::audio` uses for dictation.
/// * `System`: the default *output* device opened as an input stream, which
///   cpal's WASAPI host turns into a loopback capture
///   (`AUDCLNT_STREAMFLAGS_LOOPBACK`), using the device's default output config.
pub fn open_source(
    source: MeetingSource,
    selected_microphone: Option<&str>,
) -> Result<CaptureStream, String> {
    match source {
        MeetingSource::Microphone => open_microphone(selected_microphone),
        MeetingSource::System => open_system_loopback(),
    }
}

fn open_microphone(selected: Option<&str>) -> Result<CaptureStream, String> {
    let host = crate::audio_toolkit::get_cpal_host();
    let mut device = None;
    if let Some(name) = selected {
        match crate::audio_toolkit::list_input_devices() {
            Ok(devices) => {
                device = devices
                    .into_iter()
                    .find(|d| d.name == name)
                    .map(|d| d.device);
                if device.is_none() {
                    log::warn!("Meeting: selected microphone '{name}' not found, using default");
                }
            }
            Err(e) => log::warn!("Meeting: failed to list microphones ({e}), using default"),
        }
    }
    let device = match device {
        Some(device) => device,
        None => host
            .default_input_device()
            .ok_or_else(|| "Микрофон не найден".to_string())?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| format!("Не удалось получить настройки микрофона: {e}"))?;
    build_and_play(&device, &config)
}

fn open_system_loopback() -> Result<CaptureStream, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("Запись звука компьютера поддерживается только в Windows".to_string())
    }
    #[cfg(target_os = "windows")]
    {
        let host = crate::audio_toolkit::get_cpal_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "Устройство вывода звука не найдено".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("Не удалось получить настройки вывода звука: {e}"))?;
        build_and_play(&device, &config)
    }
}

fn build_and_play(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
) -> Result<CaptureStream, String> {
    let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());
    let sample_rate = config.sample_rate().0;
    let channels = (config.channels() as usize).max(1);
    log::info!(
        "Meeting capture: device={device_name:?} rate={sample_rate} channels={channels} format={:?}",
        config.sample_format()
    );

    let flags = Arc::new(CaptureFlags::default());
    let result = match config.sample_format() {
        cpal::SampleFormat::U8 => build_stream::<u8>(device, config, channels, &flags),
        cpal::SampleFormat::I8 => build_stream::<i8>(device, config, channels, &flags),
        cpal::SampleFormat::I16 => build_stream::<i16>(device, config, channels, &flags),
        cpal::SampleFormat::I32 => build_stream::<i32>(device, config, channels, &flags),
        cpal::SampleFormat::F32 => build_stream::<f32>(device, config, channels, &flags),
        other => return Err(format!("Неподдерживаемый формат звука: {other:?}")),
    };
    let (stream, consumer) =
        result.map_err(|e| format!("Не удалось открыть «{device_name}»: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("Не удалось запустить «{device_name}»: {e}"))?;

    Ok(CaptureStream {
        _stream: stream,
        consumer,
        sample_rate,
        device_name,
        flags,
    })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: usize,
    flags: &Arc<CaptureFlags>,
) -> Result<(cpal::Stream, Consumer<f32>), cpal::BuildStreamError>
where
    T: Sample + SizedSample + Copy + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let capacity = (config.sample_rate().0 as usize).max(8_000) * RING_SECONDS;
    let (mut producer, consumer) = RingBuffer::<f32>::new(capacity);

    let data_flags = Arc::clone(flags);
    let error_flags = Arc::clone(flags);
    let stream = device.build_input_stream(
        &config.clone().into(),
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            write_mono(data, channels, &mut producer, &data_flags);
        },
        move |_err| {
            // Keep the error callback trivial; the processing thread reports it.
            error_flags.stream_error.store(true, Ordering::Release);
        },
        None,
    )?;
    Ok((stream, consumer))
}

/// Real-time callback body: down-mix interleaved frames to mono and push them
/// into the ring. Allocation-, lock- and logging-free.
fn write_mono<T>(data: &[T], channels: usize, producer: &mut Producer<f32>, flags: &CaptureFlags)
where
    T: Sample + SizedSample + Copy,
    f32: cpal::FromSample<T>,
{
    let frame_count = data.len() / channels;
    let writable = producer.slots().min(frame_count);
    let mut written = 0;
    if writable > 0 {
        if let Ok(chunk) = producer.write_chunk_uninit(writable) {
            written = if channels == 1 {
                chunk.fill_from_iter(
                    data.iter()
                        .take(writable)
                        .map(|&sample| sample.to_sample::<f32>()),
                )
            } else {
                chunk.fill_from_iter(data.chunks_exact(channels).take(writable).map(|frame| {
                    frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32
                }))
            };
        }
    }
    let dropped = frame_count.saturating_sub(written);
    if dropped > 0 {
        flags
            .overrun_samples
            .fetch_add(dropped as u64, Ordering::Relaxed);
    }
}

/// RMS of a block mapped to a 0..=1 meter value (-60 dBFS → 0, 0 dBFS → 1).
pub fn meter_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    if rms <= 1e-6 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_level_maps_db_range() {
        assert_eq!(meter_level(&[]), 0.0);
        assert_eq!(meter_level(&[0.0; 16]), 0.0);
        let full = meter_level(&[1.0, -1.0, 1.0, -1.0]);
        assert!((full - 1.0).abs() < 1e-4);
        // 0.001 RMS = -60 dBFS → bottom of the meter.
        assert!(meter_level(&[0.001; 16]) < 0.01);
        let mid = meter_level(&[0.0316; 16]); // ≈ -30 dBFS
        assert!((mid - 0.5).abs() < 0.02);
    }

    #[test]
    fn write_mono_downmixes_and_counts_overruns() {
        let flags = CaptureFlags::default();
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(2);
        // 3 stereo frames into a 2-slot ring: 2 written, 1 dropped.
        write_mono(&[1.0f32, 0.0, 0.5, 0.5, -1.0, -1.0], 2, &mut producer, &flags);
        assert_eq!(consumer.pop().unwrap(), 0.5);
        assert_eq!(consumer.pop().unwrap(), 0.5);
        assert!(consumer.pop().is_err());
        assert_eq!(flags.overrun_samples.load(Ordering::Relaxed), 1);
    }
}
