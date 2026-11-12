//! Automatic microphone gain.
//!
//! Microphones differ enormously in level, and Windows often leaves the
//! capture volume low. Without correction the other side hears a faint
//! voice. [`AutoGain`] raises quiet speech towards a comfortable level,
//! leaves loud speech alone, never amplifies the background while nobody
//! speaks, and changes the gain smoothly so the change itself is not heard.

/// Peak level speech is raised towards (1.0 is full scale).
pub const TARGET_PEAK: f32 = 0.5;
/// The most a quiet microphone is amplified (18 dB).
pub const MAX_GAIN: f32 = 8.0;
/// Below this peak a block is treated as background, not speech.
pub const NOISE_FLOOR: f32 = 0.01;

/// Amplified audio stays below this, leaving a little room under full scale.
const CEILING: f32 = 0.9;

/// How long the remembered speech level takes to fade, in seconds.
const LEVEL_DECAY_SECONDS: f32 = 2.0;
/// How fast the gain may rise and fall, in seconds.
const RISE_SECONDS: f32 = 0.5;
const FALL_SECONDS: f32 = 0.01;

#[derive(Debug)]
pub struct AutoGain {
    sample_rate: f32,
    gain: f32,
    /// Recent peak of speech, before gain.
    level: f32,
}

impl AutoGain {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1) as f32,
            gain: 1.0,
            level: 0.0,
        }
    }

    /// The gain applied at the end of the last block.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Adjusts one block of microphone samples in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }
        let seconds = samples.len() as f32 / self.sample_rate;
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));

        // The gain the block starts with must already be safe for its peak.
        // On a sudden loud sound that is a step down, hidden by the sound's
        // own onset; ramping down slowly instead would clip its beginning.
        let start = if peak > 0.0 {
            self.gain.min(1.0_f32.max(CEILING / peak))
        } else {
            self.gain
        };
        if peak > NOISE_FLOOR {
            // Remember the loudest recent speech; forget it slowly.
            let decay = (-seconds / LEVEL_DECAY_SECONDS).exp();
            self.level = peak.max(self.level * decay);
            let wanted = (TARGET_PEAK / self.level).clamp(1.0, MAX_GAIN);
            let time = if wanted < self.gain {
                FALL_SECONDS
            } else {
                RISE_SECONDS
            };
            self.gain += (wanted - self.gain) * (1.0 - (-seconds / time).exp());
            // Whatever the smoothing says, this block must not clip.
            self.gain = self.gain.min(1.0_f32.max(CEILING / peak));
        }
        // Background only: keep the gain, so the noise is not pumped up.

        let end = self.gain;
        let last = (samples.len() - 1).max(1) as f32;
        for (index, sample) in samples.iter_mut().enumerate() {
            // Move from the old gain to the new one across the block.
            let gain = start + (end - start) * (index as f32 / last);
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const RATE: u32 = 48_000;
    const BLOCK: usize = 480; // 10 ms

    /// Runs a 300 Hz tone of the given peak through the gain and returns
    /// every processed block.
    fn run(gain: &mut AutoGain, peak: f32, blocks: usize, offset: usize) -> Vec<Vec<f32>> {
        (0..blocks)
            .map(|block| {
                let mut samples: Vec<f32> = (0..BLOCK)
                    .map(|index| {
                        let time = (offset + block * BLOCK + index) as f32 / RATE as f32;
                        peak * (2.0 * PI * 300.0 * time).sin()
                    })
                    .collect();
                gain.process(&mut samples);
                samples
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn quiet_speech_is_raised_to_the_target_level() {
        let mut gain = AutoGain::new(RATE);
        // A microphone that only reaches 10 % of full scale.
        let output = run(&mut gain, 0.1, 300, 0);
        let settled = peak(output.last().unwrap());
        assert!((settled - TARGET_PEAK).abs() < 0.02, "peak {settled}");
        assert!((gain.gain() - 5.0).abs() < 0.2, "gain {}", gain.gain());
    }

    #[test]
    fn very_quiet_speech_gets_at_most_the_maximum_gain() {
        let mut gain = AutoGain::new(RATE);
        let output = run(&mut gain, 0.02, 500, 0);
        assert!(
            (gain.gain() - MAX_GAIN).abs() < 0.05,
            "gain {}",
            gain.gain()
        );
        let settled = peak(output.last().unwrap());
        assert!((settled - 0.16).abs() < 0.01, "peak {settled}");
    }

    #[test]
    fn loud_speech_is_left_alone() {
        let mut gain = AutoGain::new(RATE);
        let output = run(&mut gain, 0.8, 200, 0);
        assert_eq!(gain.gain(), 1.0);
        assert!((peak(output.last().unwrap()) - 0.8).abs() < 0.001);
    }

    #[test]
    fn background_noise_is_never_amplified() {
        let mut gain = AutoGain::new(RATE);
        let output = run(&mut gain, 0.005, 500, 0);
        assert_eq!(gain.gain(), 1.0);
        assert!((peak(output.last().unwrap()) - 0.005).abs() < 0.0001);
    }

    #[test]
    fn gain_is_held_during_pauses_between_words() {
        let mut gain = AutoGain::new(RATE);
        run(&mut gain, 0.1, 300, 0);
        let speaking = gain.gain();
        // Three seconds of background only.
        let output = run(&mut gain, 0.004, 300, 0);
        assert_eq!(gain.gain(), speaking);
        // The background got the same gain as the speech, not more.
        assert!(peak(output.last().unwrap()) <= 0.004 * speaking + 0.0001);
    }

    #[test]
    fn sudden_loud_speech_after_quiet_speech_does_not_clip() {
        let mut gain = AutoGain::new(RATE);
        run(&mut gain, 0.05, 400, 0);
        assert!(gain.gain() > 7.0);

        // Ten times louder from one block to the next.
        let output = run(&mut gain, 0.5, 30, 0);
        for block in &output {
            assert!(peak(block) <= 1.0);
        }
        // No flat tops: the very first loud block is already within range
        // without the hard limit having to cut it.
        let clipped = output[0]
            .iter()
            .filter(|sample| sample.abs() >= 1.0)
            .count();
        assert!(clipped <= 2, "{clipped} clipped samples");
        // And within a tenth of a second the level is back at the target.
        let settled = peak(&output[10]);
        assert!((settled - TARGET_PEAK).abs() < 0.05, "peak {settled}");
    }

    #[test]
    fn gain_changes_are_spread_over_the_block_without_steps() {
        let mut gain = AutoGain::new(RATE);
        let output = run(&mut gain, 0.1, 100, 0);
        let all: Vec<f32> = output.into_iter().flatten().collect();
        // A 300 Hz tone at the target peak changes by at most this much per
        // sample; a step in the gain would show as a larger jump.
        let limit = TARGET_PEAK * 2.0 * PI * 300.0 / RATE as f32 * 1.05;
        for pair in all.windows(2) {
            assert!((pair[1] - pair[0]).abs() <= limit);
        }
    }

    #[test]
    fn empty_block_changes_nothing() {
        let mut gain = AutoGain::new(RATE);
        gain.process(&mut []);
        assert_eq!(gain.gain(), 1.0);
    }
}
