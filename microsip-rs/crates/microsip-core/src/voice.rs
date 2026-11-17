//! Cleaning the microphone's sound before it is sent: echo cancellation
//! (what the speaker plays and the microphone picks up again, so that the
//! other side does not hear itself) and noise suppression (fans, street,
//! keyboard).
//!
//! Both come from `sonora`, a Rust version of the audio processing of
//! WebRTC (its echo canceller AEC3 and its noise suppressor). It works on
//! 10 ms frames; [`VoiceProcessor`] collects the blocks of any size the
//! sound card and the network deliver into such frames.
//!
//! The echo canceller has to know what is played: the decoded voice of the
//! other side is handed to [`VoiceProcessor::played`] as it arrives, the
//! microphone to [`VoiceProcessor::clean`]. The delay between the two is
//! found by the canceller itself.

use sonora::config::{
    EchoCanceller, HighPassFilter, MaxProcessingRate, NoiseSuppression, NoiseSuppressionLevel,
    Pipeline, TransparentModeType,
};
use sonora::{AudioProcessing, Config, StreamConfig};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Which cleaning is switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceSettings {
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
}

impl VoiceSettings {
    pub fn any(self) -> bool {
        self.echo_cancellation || self.noise_suppression
    }
}

/// Processed microphone sound waiting beyond this is dropped (100 ms), so a
/// hiccup cannot make the delay grow.
const READY_LIMIT_FRAMES: usize = 10;

struct Inner {
    processing: AudioProcessing,
    capture: StreamConfig,
    render: StreamConfig,
    /// Microphone samples not yet making a whole frame.
    capture_pending: Vec<f32>,
    /// Cleaned microphone samples waiting to be handed back.
    ready: VecDeque<f32>,
    /// Played samples not yet making a whole frame.
    render_pending: Vec<f32>,
    frame_out: Vec<f32>,
    render_out: Vec<f32>,
}

/// The cleaning of one call. Clones share it: one is used on the sound
/// card's thread, one on the thread receiving the other side's voice.
#[derive(Clone)]
pub struct VoiceProcessor {
    inner: Arc<Mutex<Inner>>,
}

impl VoiceProcessor {
    /// `capture_rate` is the microphone's sample rate, `render_rate` that
    /// of the voice played (the codec's). `None` when nothing is switched
    /// on.
    pub fn new(settings: VoiceSettings, capture_rate: u32, render_rate: u32) -> Option<Self> {
        if !settings.any() || capture_rate == 0 || render_rate == 0 {
            return None;
        }
        let config = Config {
            // Telephone sound needs no more than 32 kHz inside, and it
            // costs a third less than 48.
            pipeline: Pipeline {
                maximum_internal_processing_rate: MaxProcessingRate::Rate32kHz,
                ..Pipeline::default()
            },
            // Removes the rumble below speech (also needed by the echo
            // canceller).
            high_pass_filter: Some(HighPassFilter::default()),
            echo_canceller: settings.echo_cancellation.then(|| EchoCanceller {
                // With a headset there is no echo; this mode notices and
                // stops suppressing, so the voice is not thinned out.
                transparent_mode: TransparentModeType::Hmm,
                ..EchoCanceller::default()
            }),
            noise_suppression: settings.noise_suppression.then(|| NoiseSuppression {
                level: NoiseSuppressionLevel::Moderate,
                ..NoiseSuppression::default()
            }),
            ..Config::default()
        };
        let capture = StreamConfig::new(capture_rate, 1);
        let render = StreamConfig::new(render_rate, 1);
        let processing = AudioProcessing::builder()
            .config(config)
            .capture_config(capture)
            .render_config(render)
            .build();
        Some(Self {
            inner: Arc::new(Mutex::new(Inner {
                processing,
                capture,
                render,
                capture_pending: Vec::new(),
                ready: VecDeque::new(),
                render_pending: Vec::new(),
                frame_out: vec![0.0; capture.num_frames()],
                render_out: vec![0.0; render.num_frames()],
            })),
        })
    }

    /// Takes sound just handed to the speaker (at the render rate).
    pub fn played(&self, samples: &[f32]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let inner = &mut *inner;
        let frame = inner.render.num_frames();
        inner.render_pending.extend_from_slice(samples);
        while inner.render_pending.len() >= frame {
            let input: Vec<f32> = inner.render_pending.drain(..frame).collect();
            let output = &mut inner.render_out;
            // The played sound only teaches the canceller; it is not changed.
            let _ = inner
                .processing
                .process_render_f32(&[&input], &mut [&mut output[..]]);
        }
    }

    /// Cleans microphone sound in place (at the capture rate). The block
    /// keeps its length; the cleaned sound is up to 10 ms later, the time
    /// needed to collect a whole frame.
    pub fn clean(&self, samples: &mut [f32]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let inner = &mut *inner;
        let frame = inner.capture.num_frames();
        inner.capture_pending.extend_from_slice(samples);
        while inner.capture_pending.len() >= frame {
            let input: Vec<f32> = inner.capture_pending.drain(..frame).collect();
            let output = &mut inner.frame_out;
            match inner
                .processing
                .process_capture_f32(&[&input], &mut [&mut output[..]])
            {
                Ok(()) => inner.ready.extend(output.iter().copied()),
                // Never silence the user because the cleaning failed.
                Err(_) => inner.ready.extend(input.iter().copied()),
            }
        }
        let limit = READY_LIMIT_FRAMES * frame;
        if inner.ready.len() > limit {
            let excess = inner.ready.len() - limit;
            inner.ready.drain(..excess);
        }
        for sample in samples.iter_mut() {
            // At the very start there is nothing cleaned yet: silence.
            *sample = inner.ready.pop_front().unwrap_or(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sound with the rhythm of speech: voice-band noise in syllables of
    /// 150 ms with pauses between them. (Steady tones would not do: both
    /// the echo canceller and the noise suppressor leave steady sounds
    /// alone, rightly, as they are not speech.) `seed` makes another voice.
    fn speech_like(rate: u32, seconds: f32, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(7);
        let mut low = 0.0_f32;
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .map(|index| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = (state >> 16) as f32 / 32_768.0 - 1.0;
                low = low * 0.7 + noise * 0.3;
                let time = index as f32 / rate as f32 + seed as f32 * 0.11;
                let syllable = (time * 4.0).fract();
                let envelope = if syllable < 0.6 {
                    (syllable / 0.6 * std::f32::consts::PI).sin()
                } else {
                    0.0
                };
                low * envelope * 0.5
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    const ECHO_ONLY: VoiceSettings = VoiceSettings {
        echo_cancellation: true,
        noise_suppression: false,
    };

    /// Plays `far` through a room that sends it back to the microphone
    /// 60 ms later and quieter, adds `near` (the user), and returns what
    /// the processor makes of the microphone. The microphone runs at
    /// 48 kHz in blocks of 441 samples, the far side at 8 kHz in 20 ms
    /// packets, as in a real call.
    fn call(settings: VoiceSettings, far: &[f32], near: &[f32]) -> Vec<f32> {
        let processor = VoiceProcessor::new(settings, 48_000, 8_000).unwrap();
        // The far side's voice as the speaker plays it, at 48 kHz.
        let played: Vec<f32> = far.iter().flat_map(|sample| [*sample; 6]).collect();
        let delay = 48 * 60;
        let mut cleaned = Vec::new();
        let mut packets = far.chunks(160);
        let mut position = 0;
        while position + 441 <= near.len() {
            // A packet of the far side arrives about every 20 ms.
            while (packets.len() as f32) > ((near.len() - position) as f32 / 960.0) {
                match packets.next() {
                    Some(packet) => processor.played(packet),
                    None => break,
                }
            }
            let mut block: Vec<f32> = (position..position + 441)
                .map(|index| {
                    let echo = index
                        .checked_sub(delay)
                        .and_then(|at| played.get(at))
                        .copied()
                        .unwrap_or(0.0);
                    near[index] + 0.4 * echo
                })
                .collect();
            processor.clean(&mut block);
            cleaned.extend(block);
            position += 441;
        }
        cleaned
    }

    #[test]
    fn the_echo_of_the_other_side_is_taken_out() {
        let far = speech_like(8_000, 8.0, 1);
        let silence = vec![0.0; 48_000 * 8];
        let cleaned = call(ECHO_ONLY, &far, &silence);
        // The user says nothing: after the canceller has learnt the room
        // (the first seconds), little of the echo is left.
        let echo: Vec<f32> = far.iter().flat_map(|sample| [*sample * 0.4; 6]).collect();
        let tail = 48_000 * 4;
        let before = rms(&echo[tail..cleaned.len()]);
        let after = rms(&cleaned[tail..]);
        let reduction = 20.0 * (before / after.max(1e-9)).log10();
        assert!(
            reduction > 15.0,
            "the echo was lowered by {reduction:.1} dB only"
        );
    }

    #[test]
    fn the_users_own_voice_goes_through_the_echo_canceller() {
        // The user speaks while the other side is silent.
        let far = vec![0.0; 8_000 * 6];
        let near = speech_like(48_000, 6.0, 2);
        let cleaned = call(ECHO_ONLY, &far, &near);
        let tail = 48_000 * 2;
        let ratio = rms(&cleaned[tail..]) / rms(&near[tail..cleaned.len()]);
        assert!(
            ratio > 0.7,
            "the voice came out at {:.0} % of its level",
            ratio * 100.0
        );
    }

    #[test]
    fn noise_in_the_pauses_is_lowered_and_the_voice_kept() {
        let settings = VoiceSettings {
            echo_cancellation: false,
            noise_suppression: true,
        };
        let processor = VoiceProcessor::new(settings, 16_000, 16_000).unwrap();
        // A fan under the speech: steady noise, the same at every run.
        let mut state = 12_345_u32;
        let mut fan = || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((state >> 16) as f32 / 32_768.0 - 1.0) * 0.01
        };
        let voice = speech_like(16_000, 8.0, 3);
        let noisy: Vec<f32> = voice.iter().map(|sample| sample + fan()).collect();
        let mut cleaned = Vec::new();
        for block in noisy.chunks(160) {
            let mut block = block.to_vec();
            processor.clean(&mut block);
            cleaned.extend(block);
        }
        // Compared after the first two seconds, and allowing for the 10 ms
        // the processing takes.
        let shift = 160;
        let (mut pause_before, mut pause_after, mut voice_before, mut voice_after) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for index in 32_000..noisy.len() - shift {
            let syllable = ((index as f32 / 16_000.0 + 3.0 * 0.11) * 4.0).fract();
            if syllable > 0.65 && syllable < 0.95 {
                pause_before.push(noisy[index]);
                pause_after.push(cleaned[index + shift]);
            } else if (0.15..0.45).contains(&syllable) {
                voice_before.push(noisy[index]);
                voice_after.push(cleaned[index + shift]);
            }
        }
        let noise_drop = 20.0 * (rms(&pause_before) / rms(&pause_after).max(1e-9)).log10();
        assert!(
            noise_drop > 6.0,
            "the noise was lowered by {noise_drop:.1} dB only"
        );
        let kept = rms(&voice_after) / rms(&voice_before);
        assert!(kept > 0.7, "the voice came out at {:.0} %", kept * 100.0);
    }

    #[test]
    fn blocks_keep_their_length_and_nothing_is_on_returns_none() {
        let off = VoiceSettings {
            echo_cancellation: false,
            noise_suppression: false,
        };
        assert!(VoiceProcessor::new(off, 48_000, 8_000).is_none());
        assert!(VoiceProcessor::new(ECHO_ONLY, 0, 8_000).is_none());

        // A sound card with 44.1 kHz and odd blocks.
        let processor = VoiceProcessor::new(ECHO_ONLY, 44_100, 16_000).unwrap();
        let mut total = 0;
        for size in [1, 37, 441, 512, 1_000, 3] {
            let mut block = vec![0.1; size];
            processor.clean(&mut block);
            assert_eq!(block.len(), size);
            assert!(block.iter().all(|sample| sample.is_finite()));
            total += size;
            processor.played(&vec![0.0; 320]);
        }
        assert_eq!(total, 1_994);
        // A clone works on the same processing.
        let other = processor.clone();
        let mut block = vec![0.1; 441];
        other.clean(&mut block);
    }
}
