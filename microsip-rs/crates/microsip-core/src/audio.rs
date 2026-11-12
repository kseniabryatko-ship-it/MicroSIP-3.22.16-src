use crate::playout::PlayoutBuffer;
use crate::resample::Resampler;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum AudioError {
    NoInputDevice,
    NoOutputDevice,
    UnsupportedFormat(&'static str),
    Configuration(String),
    Stream(String),
    Channel(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoInputDevice => formatter.write_str("No default microphone was found"),
            Self::NoOutputDevice => formatter.write_str("No default speaker was found"),
            Self::UnsupportedFormat(direction) => {
                write!(formatter, "Unsupported {direction} sample format")
            }
            Self::Configuration(error) => {
                write!(formatter, "Audio configuration error: {error}")
            }
            Self::Stream(error) => {
                write!(formatter, "Audio stream error: {error}")
            }
            Self::Channel(error) => {
                write!(formatter, "Audio channel error: {error}")
            }
        }
    }
}

impl Error for AudioError {}

/// Преобразование поддерживаемых форматов CPAL
/// в нормализованный диапазон `[-1.0, 1.0]`.
trait AudioSample: Copy + Send + 'static {
    fn to_f32(self) -> f32;

    fn from_f32(value: f32) -> Self;
}

impl AudioSample for i8 {
    fn to_f32(self) -> f32 {
        self as f32 / 127.0
    }

    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 127.0).round() as i8
    }
}

impl AudioSample for i16 {
    fn to_f32(self) -> f32 {
        self as f32 / 32_767.0
    }

    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 32_767.0).round() as i16
    }
}

impl AudioSample for i32 {
    fn to_f32(self) -> f32 {
        self as f32 / 2_147_483_647.0
    }

    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32
    }
}

impl AudioSample for f32 {
    fn to_f32(self) -> f32 {
        self
    }

    fn from_f32(value: f32) -> Self {
        value.clamp(-1.0, 1.0)
    }
}

impl AudioSample for u8 {
    fn to_f32(self) -> f32 {
        (self as f32 - 128.0) / 128.0
    }

    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 128.0 + 128.0)
            .round()
            .clamp(0.0, 255.0) as u8
    }
}

impl AudioSample for u16 {
    fn to_f32(self) -> f32 {
        (self as f32 - 32_768.0) / 32_768.0
    }

    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 32_768.0 + 32_768.0)
            .round()
            .clamp(0.0, 65_535.0) as u16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Highest volume setting, in percent.
pub const MAX_VOLUME_PERCENT: u32 = 200;

/// Multiplier for a volume setting in percent; 100 leaves the sound as it is.
pub fn volume_factor(percent: u32) -> f32 {
    percent.min(MAX_VOLUME_PERCENT) as f32 / 100.0
}

/// Names of the microphones the system offers.
pub fn input_device_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.filter_map(|device| device.name().ok()).collect())
        .unwrap_or_default()
}

/// Names of the speakers and headphones the system offers.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|device| device.name().ok()).collect())
        .unwrap_or_default()
}

/// The named microphone, or the system default when no name is given or the
/// named one is not connected any more.
fn input_device(name: Option<&str>) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    let named = name.filter(|name| !name.is_empty()).and_then(|name| {
        host.input_devices()
            .ok()?
            .find(|device| device.name().is_ok_and(|found| found == name))
    });
    named
        .or_else(|| host.default_input_device())
        .ok_or(AudioError::NoInputDevice)
}

/// The named speaker, or the system default; see [`input_device`].
fn output_device(name: Option<&str>) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    let named = name.filter(|name| !name.is_empty()).and_then(|name| {
        host.output_devices()
            .ok()?
            .find(|device| device.name().is_ok_and(|found| found == name))
    });
    named
        .or_else(|| host.default_output_device())
        .ok_or(AudioError::NoOutputDevice)
}

pub struct Microphone {
    _stream: cpal::Stream,
    format: AudioFormat,
    /// `None` when the samples go to a sink given to [`Microphone::start_with`].
    receiver: Option<Receiver<Vec<f32>>>,
}

impl Microphone {
    /// Format of the default microphone, without starting it.
    pub fn input_format() -> Result<AudioFormat, AudioError> {
        Self::input_format_of(None)
    }

    /// Format of the named microphone (`None`: the system default).
    pub fn input_format_of(device: Option<&str>) -> Result<AudioFormat, AudioError> {
        let supported = input_device(device)?
            .default_input_config()
            .map_err(|error| AudioError::Configuration(error.to_string()))?;
        Ok(AudioFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        })
    }

    /// Starts the microphone; samples are read with [`Microphone::read_chunk`].
    pub fn start() -> Result<Self, AudioError> {
        let (sender, receiver) = mpsc::channel();
        let mut microphone = Self::start_with(move |samples| {
            let _ = sender.send(samples);
        })?;
        microphone.receiver = Some(receiver);
        Ok(microphone)
    }

    /// Starts the microphone and hands every captured block of mono samples
    /// to `sink`, on the sound card's own thread. The sink must not block.
    pub fn start_with<F>(sink: F) -> Result<Self, AudioError>
    where
        F: FnMut(Vec<f32>) + Send + 'static,
    {
        Self::start_on(None, sink)
    }

    /// Like [`Microphone::start_with`], on the named microphone.
    pub fn start_on<F>(device: Option<&str>, sink: F) -> Result<Self, AudioError>
    where
        F: FnMut(Vec<f32>) + Send + 'static,
    {
        let device = input_device(device)?;

        let supported = device
            .default_input_config()
            .map_err(|error| AudioError::Configuration(error.to_string()))?;

        let sample_format = supported.sample_format();

        let format = AudioFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };

        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_input_stream::<i8, F>(&device, supported.into(), sink)?,
            cpal::SampleFormat::I16 => {
                build_input_stream::<i16, F>(&device, supported.into(), sink)?
            }
            cpal::SampleFormat::I32 => {
                build_input_stream::<i32, F>(&device, supported.into(), sink)?
            }
            cpal::SampleFormat::F32 => {
                build_input_stream::<f32, F>(&device, supported.into(), sink)?
            }
            cpal::SampleFormat::U8 => build_input_stream::<u8, F>(&device, supported.into(), sink)?,
            cpal::SampleFormat::U16 => {
                build_input_stream::<u16, F>(&device, supported.into(), sink)?
            }
            _ => {
                return Err(AudioError::UnsupportedFormat("microphone"));
            }
        };

        stream
            .play()
            .map_err(|error| AudioError::Stream(error.to_string()))?;

        Ok(Self {
            _stream: stream,
            format,
            receiver: None,
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn read_chunk(&self) -> Result<Option<Vec<f32>>, AudioError> {
        let Some(receiver) = self.receiver.as_ref() else {
            return Ok(None);
        };
        match receiver.try_recv() {
            Ok(samples) => Ok(Some(samples)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(AudioError::Channel(
                "Microphone stream was disconnected".to_owned(),
            )),
        }
    }
}

pub struct Speaker {
    _stream: cpal::Stream,
    format: AudioFormat,
    /// Shared with the sound card's callback.
    playout: Arc<Mutex<PlayoutBuffer>>,
    /// Used by [`Speaker::play_mono`].
    own_sink: RefCell<SpeakerSink>,
}

/// The input side of a [`Speaker`]. It can be moved to another thread, so
/// received audio reaches the sound card without passing through anything
/// that might be busy.
pub struct SpeakerSink {
    playout: Arc<Mutex<PlayoutBuffer>>,
    device_rate: u32,
    /// Kept between calls so consecutive packets join without a click.
    resampler: Option<(u32, Resampler)>,
}

impl SpeakerSink {
    /// Queues mono samples recorded at `source_sample_rate` for playback.
    pub fn push(&mut self, samples: &[f32], source_sample_rate: u32) -> Result<(), AudioError> {
        // One converter per stream; a new one only if the source rate changes.
        if self
            .resampler
            .as_ref()
            .is_none_or(|(rate, _)| *rate != source_sample_rate)
        {
            let created = Resampler::new(source_sample_rate, self.device_rate)
                .ok_or_else(|| AudioError::Configuration("Invalid sample rate".to_owned()))?;
            self.resampler = Some((source_sample_rate, created));
        }
        let converted = match self.resampler.as_mut() {
            Some((_, resampler)) => resampler.push(samples),
            None => return Ok(()),
        };
        self.playout
            .lock()
            .map_err(|_| AudioError::Channel("Speaker buffer is unusable".to_owned()))?
            .push(&converted);
        Ok(())
    }
}

impl Speaker {
    pub fn start() -> Result<Self, AudioError> {
        Self::start_on(None, Arc::new(AtomicU32::new(100)), 0)
    }

    /// Starts the named speaker (`None`: the system default). `volume` is a
    /// percentage that may be changed while the speaker plays; `reserve_ms`
    /// is the playback reserve to begin with (0: the usual one).
    pub fn start_on(
        device: Option<&str>,
        volume: Arc<AtomicU32>,
        reserve_ms: u32,
    ) -> Result<Self, AudioError> {
        let device = output_device(device)?;

        let supported = device
            .default_output_config()
            .map_err(|error| AudioError::Configuration(error.to_string()))?;

        let sample_format = supported.sample_format();

        let format = AudioFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };

        let playout = Arc::new(Mutex::new(PlayoutBuffer::adaptive_from(
            format.sample_rate,
            reserve_ms,
        )));
        let shared = || (Arc::clone(&playout), Arc::clone(&volume));

        let stream = match sample_format {
            cpal::SampleFormat::I8 => {
                build_output_stream::<i8>(&device, supported.into(), shared())?
            }
            cpal::SampleFormat::I16 => {
                build_output_stream::<i16>(&device, supported.into(), shared())?
            }
            cpal::SampleFormat::I32 => {
                build_output_stream::<i32>(&device, supported.into(), shared())?
            }
            cpal::SampleFormat::F32 => {
                build_output_stream::<f32>(&device, supported.into(), shared())?
            }
            cpal::SampleFormat::U8 => {
                build_output_stream::<u8>(&device, supported.into(), shared())?
            }
            cpal::SampleFormat::U16 => {
                build_output_stream::<u16>(&device, supported.into(), shared())?
            }
            _ => {
                return Err(AudioError::UnsupportedFormat("speaker"));
            }
        };

        stream
            .play()
            .map_err(|error| AudioError::Stream(error.to_string()))?;

        let own_sink = RefCell::new(SpeakerSink {
            playout: Arc::clone(&playout),
            device_rate: format.sample_rate,
            resampler: None,
        });
        Ok(Self {
            _stream: stream,
            format,
            playout,
            own_sink,
        })
    }

    /// A separate input for this speaker that another thread may own. Use
    /// either it or [`Speaker::play_mono`] for one stream, not both.
    pub fn sink(&self) -> SpeakerSink {
        SpeakerSink {
            playout: Arc::clone(&self.playout),
            device_rate: self.format.sample_rate,
            resampler: None,
        }
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn play_mono(&self, samples: &[f32], source_sample_rate: u32) -> Result<(), AudioError> {
        self.own_sink.borrow_mut().push(samples, source_sample_rate)
    }

    /// The playback reserve in use now, in milliseconds.
    pub fn reserve_ms(&self) -> u32 {
        self.playout
            .lock()
            .map(|playout| playout.target_ms())
            .unwrap_or(0)
    }

    /// The largest playback reserve the connection has needed, in milliseconds.
    pub fn largest_reserve_ms(&self) -> u32 {
        self.playout
            .lock()
            .map(|playout| playout.largest_target_ms())
            .unwrap_or(0)
    }

    /// How many times playback ran out of audio since the speaker started.
    pub fn underruns(&self) -> u64 {
        self.playout
            .lock()
            .map(|playout| playout.underruns())
            .unwrap_or(0)
    }
}

fn build_input_stream<S, F>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut sink: F,
) -> Result<cpal::Stream, AudioError>
where
    S: cpal::SizedSample + AudioSample,
    F: FnMut(Vec<f32>) + Send + 'static,
{
    let channels = config.channels as usize;

    if channels == 0 {
        return Err(AudioError::Configuration(
            "Microphone reported zero channels".to_owned(),
        ));
    }

    let stream = device
        .build_input_stream(
            &config,
            move |data: &[S], _: &cpal::InputCallbackInfo| {
                sink(mix_to_mono(data, channels));
            },
            move |error| {
                eprintln!("Microphone callback error: {error}");
            },
            None,
        )
        .map_err(|error| AudioError::Stream(error.to_string()))?;

    Ok(stream)
}

/// One sample per frame: the channel with the strongest signal. Many
/// "stereo" microphones carry the voice on one channel only; averaging the
/// channels would then halve the level.
fn mix_to_mono<S: AudioSample>(data: &[S], channels: usize) -> Vec<f32> {
    data.chunks_exact(channels)
        .map(|frame| {
            frame
                .iter()
                .map(|sample| S::to_f32(*sample))
                .fold(0.0_f32, |strongest, sample| {
                    if sample.abs() > strongest.abs() {
                        sample
                    } else {
                        strongest
                    }
                })
        })
        .collect()
}

fn build_output_stream<S>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    (playout, volume): (Arc<Mutex<PlayoutBuffer>>, Arc<AtomicU32>),
) -> Result<cpal::Stream, AudioError>
where
    S: cpal::SizedSample + AudioSample,
{
    let channels = config.channels as usize;

    if channels == 0 {
        return Err(AudioError::Configuration(
            "Speaker reported zero channels".to_owned(),
        ));
    }

    let mut mono = Vec::<f32>::new();

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [S], _: &cpal::OutputCallbackInfo| {
                mono.resize(data.len() / channels, 0.0);
                match playout.lock() {
                    Ok(mut playout) => playout.pull(&mut mono),
                    Err(_) => mono.fill(0.0),
                }
                // Applied here, at the last moment, so the slider acts at once.
                let factor = volume_factor(volume.load(Ordering::Relaxed));
                for (frame, sample) in data.chunks_exact_mut(channels).zip(&mono) {
                    frame.fill(S::from_f32(*sample * factor));
                }
            },
            move |error| {
                eprintln!("Speaker callback error: {error}");
            },
            None,
        )
        .map_err(|error| AudioError::Stream(error.to_string()))?;

    Ok(stream)
}

/// One-shot conversion, kept for the tests below.
#[cfg(test)]
fn resample_mono(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    match Resampler::new(source_rate, target_rate) {
        Some(mut resampler) => resampler.push(samples),
        None => samples.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_from_48k_to_24k() {
        let input = vec![0.0, 1.0, 0.0, -1.0, 0.0, 1.0, 0.0, -1.0];

        let output = resample_mono(&input, 48_000, 24_000);

        assert_eq!(output.len(), 4);
    }

    #[test]
    fn mono_mix_takes_the_strongest_channel_of_each_frame() {
        // Voice on the left channel only, as many headsets deliver it.
        let stereo = [0.5_f32, 0.0, -0.4, 0.0, 0.0, 0.0];
        assert_eq!(mix_to_mono(&stereo, 2), [0.5, -0.4, 0.0]);
        // Both channels alike: the level is unchanged.
        let stereo = [0.3_f32, 0.3, -0.2, -0.2];
        assert_eq!(mix_to_mono(&stereo, 2), [0.3, -0.2]);
        // Mono passes through; an incomplete last frame is ignored.
        assert_eq!(mix_to_mono(&[0.1_f32, 0.2], 1), [0.1, 0.2]);
        assert_eq!(mix_to_mono(&[1_i16, 2, 3], 2).len(), 1);
    }

    #[test]
    fn volume_percent_becomes_a_multiplier_with_an_upper_limit() {
        assert_eq!(volume_factor(0), 0.0);
        assert_eq!(volume_factor(50), 0.5);
        assert_eq!(volume_factor(100), 1.0);
        assert_eq!(volume_factor(200), 2.0);
        assert_eq!(volume_factor(1_000), 2.0);
    }

    #[test]
    fn keeps_samples_when_rates_match() {
        let input = vec![0.1, 0.2, 0.3];

        let output = resample_mono(&input, 48_000, 48_000);

        assert_eq!(output, input);
    }
}
