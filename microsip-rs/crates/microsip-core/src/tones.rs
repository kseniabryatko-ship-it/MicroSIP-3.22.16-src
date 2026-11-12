//! Call-progress sounds made by the phone itself: the ring of an incoming
//! call and the ringback heard while the other side's phone is ringing.
//!
//! [`ToneGenerator`] produces the samples; it holds no sound device, so the
//! patterns are tested as plain numbers.

use std::f32::consts::PI;

/// Sample rate of the generated sound.
pub const TONE_RATE: u32 = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Someone is calling this phone.
    Ringtone,
    /// The called phone is ringing.
    Ringback,
}

/// A repeating pattern: bursts of sound inside a cycle, times in milliseconds.
struct Pattern {
    frequencies: &'static [f32],
    /// Level of each frequency.
    level: f32,
    cycle_ms: u32,
    /// Start and end of every burst within the cycle.
    bursts: &'static [(u32, u32)],
}

impl Tone {
    fn pattern(self) -> Pattern {
        match self {
            // A double ring every three seconds, easy to notice.
            Tone::Ringtone => Pattern {
                frequencies: &[440.0, 480.0],
                level: 0.2,
                cycle_ms: 3_000,
                bursts: &[(0, 400), (600, 1_000)],
            },
            // The ringback used on Russian networks: 425 Hz, one second of
            // tone and four seconds of silence.
            Tone::Ringback => Pattern {
                frequencies: &[425.0],
                level: 0.15,
                cycle_ms: 5_000,
                bursts: &[(0, 1_000)],
            },
        }
    }
}

/// Fade at both ends of a burst, so it does not click.
const FADE_MS: f32 = 10.0;

#[derive(Debug)]
pub struct ToneGenerator {
    tone: Tone,
    /// Samples produced so far.
    position: u64,
}

impl ToneGenerator {
    pub fn new(tone: Tone) -> Self {
        Self { tone, position: 0 }
    }

    /// Time covered by the samples produced so far, in milliseconds.
    pub fn produced_ms(&self) -> u64 {
        self.position * 1_000 / u64::from(TONE_RATE)
    }

    /// The next `count` samples at [`TONE_RATE`].
    pub fn next_block(&mut self, count: usize) -> Vec<f32> {
        let pattern = self.tone.pattern();
        let cycle = u64::from(pattern.cycle_ms) * u64::from(TONE_RATE) / 1_000;
        let block = (0..count as u64)
            .map(|offset| {
                let position = self.position + offset;
                let in_cycle_ms = (position % cycle) as f32 * 1_000.0 / TONE_RATE as f32;
                let Some((start, end)) = pattern
                    .bursts
                    .iter()
                    .find(|(start, end)| (*start as f32..*end as f32).contains(&in_cycle_ms))
                else {
                    return 0.0;
                };
                let from_edge = (in_cycle_ms - *start as f32).min(*end as f32 - in_cycle_ms);
                let envelope = (from_edge / FADE_MS).clamp(0.0, 1.0);
                // Phase follows the absolute position, so blocks join smoothly.
                let time = position as f32 / TONE_RATE as f32;
                let sum: f32 = pattern
                    .frequencies
                    .iter()
                    .map(|frequency| (2.0 * PI * frequency * time).sin())
                    .sum();
                pattern.level * envelope * sum
            })
            .collect();
        self.position += count as u64;
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(tone: Tone, seconds: usize) -> Vec<f32> {
        ToneGenerator::new(tone).next_block(seconds * TONE_RATE as usize)
    }

    /// True for every 100 ms slice that contains sound.
    fn audible(samples: &[f32]) -> Vec<bool> {
        samples
            .chunks(800)
            .map(|slice| slice.iter().any(|sample| sample.abs() > 0.05))
            .collect()
    }

    fn strength(samples: &[f32], frequency: f32) -> f32 {
        let (mut real, mut imaginary) = (0.0_f32, 0.0_f32);
        for (index, sample) in samples.iter().enumerate() {
            let angle = 2.0 * PI * frequency * index as f32 / TONE_RATE as f32;
            real += sample * angle.cos();
            imaginary += sample * angle.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / samples.len() as f32
    }

    #[test]
    fn ringback_is_one_second_of_425_hz_every_five_seconds() {
        let samples = seconds(Tone::Ringback, 10);
        let pattern = audible(&samples);
        for (slice, on) in pattern.iter().enumerate() {
            let expected = slice % 50 < 10;
            assert_eq!(*on, expected, "at {} ms", slice * 100);
        }
        let burst = &samples[800..7_200];
        assert!((strength(burst, 425.0) - 0.15).abs() < 0.01);
        assert!(strength(burst, 440.0) < 0.02);
    }

    #[test]
    fn ringtone_is_a_double_ring_every_three_seconds() {
        let samples = seconds(Tone::Ringtone, 6);
        let on: Vec<usize> = audible(&samples)
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(slice, _)| slice)
            .collect();
        assert_eq!(on, [0, 1, 2, 3, 6, 7, 8, 9, 30, 31, 32, 33, 36, 37, 38, 39]);
        let burst = &samples[400..2_800];
        assert!((strength(burst, 440.0) - 0.2).abs() < 0.02);
        assert!((strength(burst, 480.0) - 0.2).abs() < 0.02);
    }

    #[test]
    fn sound_never_clips_and_bursts_start_and_end_softly() {
        for tone in [Tone::Ringtone, Tone::Ringback] {
            let samples = seconds(tone, 6);
            assert!(samples.iter().all(|sample| sample.abs() <= 0.5));
            // No jump larger than the tone itself makes at full level.
            let limit = 0.4 * 2.0 * PI * 480.0 / TONE_RATE as f32 + 0.02;
            for pair in samples.windows(2) {
                assert!((pair[1] - pair[0]).abs() <= limit, "{tone:?}");
            }
            assert_eq!(samples[0], 0.0);
        }
    }

    #[test]
    fn blocks_of_any_size_give_the_same_sound() {
        let whole = seconds(Tone::Ringtone, 4);
        let mut generator = ToneGenerator::new(Tone::Ringtone);
        let mut pieces = Vec::new();
        for size in [1, 159, 160, 161, 800, 7].iter().cycle() {
            if pieces.len() >= whole.len() {
                break;
            }
            pieces.extend(generator.next_block(*size));
        }
        assert_eq!(pieces[..whole.len()], whole[..]);
        assert_eq!(generator.produced_ms(), pieces.len() as u64 / 8);
    }
}
