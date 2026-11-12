use crate::playout::PlayoutBuffer;
use crate::resample::Resampler;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
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

pub struct Microphone {
    _stream: cpal::Stream,
    format: AudioFormat,
    receiver: Receiver<Vec<f32>>,
}

impl Microphone {
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();

        let device = host
            .default_input_device()
            .ok_or(AudioError::NoInputDevice)?;

        let supported = device
            .default_input_config()
            .map_err(|error| AudioError::Configuration(error.to_string()))?;

        let sample_format = supported.sample_format();

        let format = AudioFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };

        let (sender, receiver) = mpsc::channel();

        let stream = match sample_format {
            cpal::SampleFormat::I8 => build_input_stream::<i8>(&device, supported.into(), sender)?,
            cpal::SampleFormat::I16 => {
                build_input_stream::<i16>(&device, supported.into(), sender)?
            }
            cpal::SampleFormat::I32 => {
                build_input_stream::<i32>(&device, supported.into(), sender)?
            }
            cpal::SampleFormat::F32 => {
                build_input_stream::<f32>(&device, supported.into(), sender)?
            }
            cpal::SampleFormat::U8 => build_input_stream::<u8>(&device, supported.into(), sender)?,
            cpal::SampleFormat::U16 => {
                build_input_stream::<u16>(&device, supported.into(), sender)?
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
            receiver,
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn read_chunk(&self) -> Result<Option<Vec<f32>>, AudioError> {
        match self.receiver.try_recv() {
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
    /// Kept between calls so consecutive packets join without a click.
    resampler: RefCell<Option<(u32, Resampler)>>,
}

impl Speaker {
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();

        let device = host
            .default_output_device()
            .ok_or(AudioError::NoOutputDevice)?;

        let supported = device
            .default_output_config()
            .map_err(|error| AudioError::Configuration(error.to_string()))?;

        let sample_format = supported.sample_format();

        let format = AudioFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };

        let playout = Arc::new(Mutex::new(PlayoutBuffer::adaptive(format.sample_rate)));
        let shared = || Arc::clone(&playout);

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

        Ok(Self {
            _stream: stream,
            format,
            playout,
            resampler: RefCell::new(None),
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn play_mono(&self, samples: &[f32], source_sample_rate: u32) -> Result<(), AudioError> {
        let mut resampler = self.resampler.borrow_mut();
        // One converter per stream; a new one only if the source rate changes.
        if resampler
            .as_ref()
            .is_none_or(|(rate, _)| *rate != source_sample_rate)
        {
            let created = Resampler::new(source_sample_rate, self.format.sample_rate)
                .ok_or_else(|| AudioError::Configuration("Invalid sample rate".to_owned()))?;
            *resampler = Some((source_sample_rate, created));
        }
        let converted = match resampler.as_mut() {
            Some((_, resampler)) => resampler.push(samples),
            None => return Ok(()),
        };

        self.playout
            .lock()
            .map_err(|_| AudioError::Channel("Speaker buffer is unusable".to_owned()))?
            .push(&converted);
        Ok(())
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

fn build_input_stream<S>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sender: Sender<Vec<f32>>,
) -> Result<cpal::Stream, AudioError>
where
    S: cpal::SizedSample + AudioSample,
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
                let mono = data
                    .chunks_exact(channels)
                    .map(|frame| {
                        let sum: f32 = frame.iter().map(|sample| S::to_f32(*sample)).sum();

                        sum / channels as f32
                    })
                    .collect::<Vec<f32>>();

                let _ = sender.send(mono);
            },
            move |error| {
                eprintln!("Microphone callback error: {error}");
            },
            None,
        )
        .map_err(|error| AudioError::Stream(error.to_string()))?;

    Ok(stream)
}

fn build_output_stream<S>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    playout: Arc<Mutex<PlayoutBuffer>>,
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
                for (frame, sample) in data.chunks_exact_mut(channels).zip(&mono) {
                    frame.fill(S::from_f32(*sample));
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
    fn keeps_samples_when_rates_match() {
        let input = vec![0.1, 0.2, 0.3];

        let output = resample_mono(&input, 48_000, 48_000);

        assert_eq!(output, input);
    }
}
