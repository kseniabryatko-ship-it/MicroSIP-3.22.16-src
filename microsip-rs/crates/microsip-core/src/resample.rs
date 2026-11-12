//! Streaming sample-rate conversion for one audio channel.
//!
//! A windowed-sinc filter: unlike plain linear interpolation it removes what
//! the target rate cannot carry, so a microphone at 48 kHz does not fold
//! high tones into the 8 kHz telephone band as noise, and telephone audio
//! played at 48 kHz does not get a metallic edge. The converter keeps its
//! state between calls, so audio pushed in pieces comes out exactly as if it
//! had been pushed at once: no click at the seams.
//!
//! The signal is delayed by [`Resampler::delay`] source samples (about 2 ms
//! for the rates used here). The number of output samples is not delayed:
//! after `n` input samples there are `ceil(n * target / source)` of them.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// Filter quality: zero crossings of the sinc on each side of its centre.
const ZERO_CROSSINGS: f64 = 16.0;

/// The pass band ends a little below the Nyquist limit of the slower rate,
/// leaving room for the filter to roll off.
const PASS_BAND: f64 = 0.92;

#[derive(Debug)]
pub struct Resampler {
    source_rate: u64,
    target_rate: u64,
    /// Cut-off as a fraction of the source Nyquist frequency.
    cutoff: f64,
    /// Filter reach on each side, in source samples.
    half: usize,
    /// Delayed source samples; `window[0]` has index `window_start`.
    window: VecDeque<f32>,
    window_start: u64,
    /// Number of output samples produced so far.
    produced: u64,
    passthrough: bool,
}

impl Resampler {
    /// Returns `None` when either rate is zero.
    pub fn new(source_rate: u32, target_rate: u32) -> Option<Self> {
        if source_rate == 0 || target_rate == 0 {
            return None;
        }
        let ratio = f64::from(target_rate) / f64::from(source_rate);
        let cutoff = ratio.min(1.0) * PASS_BAND;
        let half = (ZERO_CROSSINGS / cutoff).ceil() as usize;
        // The stream starts with `half` samples of silence: that is the delay
        // which lets every output be computed as soon as its input arrives.
        let window = std::iter::repeat_n(0.0, half).collect();
        Some(Self {
            source_rate: u64::from(source_rate),
            target_rate: u64::from(target_rate),
            cutoff,
            half,
            window,
            window_start: 0,
            produced: 0,
            passthrough: source_rate == target_rate,
        })
    }

    /// Delay of the signal, in source samples. Zero when the rates are equal.
    pub fn delay(&self) -> usize {
        if self.passthrough { 0 } else { self.half }
    }

    /// Converts the next piece of audio.
    pub fn push(&mut self, samples: &[f32]) -> Vec<f32> {
        if self.passthrough {
            return samples.to_vec();
        }
        self.window.extend(samples.iter().copied());
        let end = self.window_start + self.window.len() as u64;
        let half = self.half as u64;

        let expected = samples.len() as u64 * self.target_rate / self.source_rate + 1;
        let mut output = Vec::with_capacity(expected as usize);
        loop {
            // Whole-number arithmetic: the position never drifts.
            let (centre, fraction) = self.position();
            // The newest sample the filter needs for this output.
            if centre + half >= end {
                break;
            }
            output.push(self.filter(centre, fraction));
            self.produced += 1;
        }

        // Drop what no later output can reach.
        let (next_centre, _) = self.position();
        let keep_from = next_centre.saturating_sub(half);
        while self.window_start < keep_from && !self.window.is_empty() {
            self.window.pop_front();
            self.window_start += 1;
        }
        output
    }

    /// Source position of the next output: whole sample and the part after it.
    fn position(&self) -> (u64, f64) {
        let scaled = self.produced * self.source_rate;
        (
            scaled / self.target_rate,
            (scaled % self.target_rate) as f64 / self.target_rate as f64,
        )
    }

    fn filter(&self, centre: u64, fraction: f64) -> f32 {
        let half = self.half as i64;
        let reach = self.half as f64;
        let mut sum = 0.0_f64;
        let mut weights = 0.0_f64;
        for offset in (1 - half)..=half {
            let index = centre as i64 + offset;
            let distance = offset as f64 - fraction;
            let weight = sinc(self.cutoff * distance) * hann(distance / reach);
            weights += weight;
            // Before the start of the stream there is silence.
            if index >= self.window_start as i64
                && let Some(sample) = self.window.get((index - self.window_start as i64) as usize)
            {
                sum += f64::from(*sample) * weight;
            }
        }
        // Dividing by the weight sum keeps a constant level exactly constant.
        (sum / weights) as f32
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Hann window on `-1.0..=1.0`, zero outside.
fn hann(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (PI * x).cos())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f64, rate: u32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|index| (2.0 * PI * frequency * index as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum: f64 = samples
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum();
        (sum / samples.len() as f64).sqrt()
    }

    fn convert(source: u32, target: u32, input: &[f32]) -> Vec<f32> {
        Resampler::new(source, target).unwrap().push(input)
    }

    #[test]
    fn output_count_follows_the_rate_ratio_without_delay() {
        // 20 ms at 48 kHz is exactly one 160-sample telephone packet.
        let mut down = Resampler::new(48_000, 8_000).unwrap();
        assert_eq!(down.push(&[0.0; 960]).len(), 160);
        assert_eq!(down.push(&[0.0; 960]).len(), 160);

        let mut up = Resampler::new(8_000, 48_000).unwrap();
        assert_eq!(up.push(&[0.0; 160]).len(), 960);
        assert_eq!(up.push(&[0.0; 160]).len(), 960);

        assert_eq!(convert(48_000, 8_000, &[0.0; 7]).len(), 2);
        assert_eq!(convert(48_000, 24_000, &[0.0; 8]).len(), 4);
        assert_eq!(convert(44_100, 8_000, &[0.0; 44_100]).len(), 8_000);
    }

    #[test]
    fn equal_rates_pass_samples_through_unchanged() {
        let input = [0.1, -0.2, 0.3];
        let mut resampler = Resampler::new(48_000, 48_000).unwrap();
        assert_eq!(resampler.push(&input), input);
        assert_eq!(resampler.delay(), 0);
    }

    #[test]
    fn zero_rates_are_refused() {
        assert!(Resampler::new(0, 8_000).is_none());
        assert!(Resampler::new(8_000, 0).is_none());
    }

    #[test]
    fn audio_pushed_in_pieces_equals_audio_pushed_at_once() {
        for (source, target) in [(8_000, 48_000), (48_000, 8_000), (44_100, 8_000)] {
            let input = tone(700.0, source, 4_000);
            let whole = convert(source, target, &input);

            let mut resampler = Resampler::new(source, target).unwrap();
            let mut pieces = Vec::new();
            // Uneven pieces, including empty and single-sample ones.
            let mut rest = input.as_slice();
            for size in [1, 0, 159, 160, 161, 7, 960, 3].iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (piece, tail) = rest.split_at((*size).min(rest.len()));
                pieces.extend(resampler.push(piece));
                rest = tail;
            }
            assert_eq!(pieces, whole, "{source} -> {target}");
        }
    }

    #[test]
    fn constant_level_stays_constant() {
        for (source, target) in [(8_000, 48_000), (48_000, 8_000), (44_100, 8_000)] {
            let output = convert(source, target, &vec![0.5; 6_000]);
            // Skip the start, where the filter still sees the silence before it.
            let settled = &output[output.len() / 2..];
            for sample in settled {
                assert!(
                    (sample - 0.5).abs() < 1e-4,
                    "{source} -> {target}: {sample}"
                );
            }
        }
    }

    #[test]
    fn telephone_tone_survives_upsampling_smoothly() {
        // 1 kHz at 8 kHz played on a 48 kHz device.
        let output = convert(8_000, 48_000, &tone(1_000.0, 8_000, 1_600));
        let settled = &output[2_000..output.len() - 500];
        let level = rms(settled);
        assert!((level - 0.5_f64.sqrt()).abs() < 0.01, "level {level}");

        // A clean 1 kHz sine at 48 kHz never jumps more than this per sample;
        // linear interpolation of 8 kHz audio breaks it at every source sample.
        let limit = 2.0 * PI * 1_000.0 / 48_000.0 * 1.02;
        for pair in settled.windows(2) {
            let jump = f64::from((pair[1] - pair[0]).abs());
            assert!(jump <= limit, "jump {jump} over {limit}");
        }
    }

    #[test]
    fn tones_above_the_telephone_band_are_removed_not_folded() {
        // 6 kHz cannot exist at 8 kHz. Taking every sixth sample would turn
        // it into a loud 2 kHz tone; the filter must silence it instead.
        let output = convert(48_000, 8_000, &tone(6_000.0, 48_000, 48_000));
        let settled = &output[400..];
        assert!(rms(settled) < 0.01, "leak {}", rms(settled));

        // Speech frequencies pass at full level.
        let output = convert(48_000, 8_000, &tone(1_000.0, 48_000, 48_000));
        let level = rms(&output[400..]);
        assert!((level - 0.5_f64.sqrt()).abs() < 0.01, "level {level}");
    }

    #[test]
    fn signal_is_delayed_by_the_reported_number_of_source_samples() {
        let mut resampler = Resampler::new(8_000, 48_000).unwrap();
        let delay = resampler.delay();
        // A single click: its peak comes out `delay` source samples later.
        let mut input = vec![0.0; 400];
        input[100] = 1.0;
        let output = resampler.push(&input);
        let peak = output
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(peak, (100 + delay) * 6);
    }
}
