//! Playout buffer between the network and the sound card.
//!
//! Voice packets do not arrive evenly: the network holds some back and then
//! delivers several at once. A sound card that plays each packet the moment
//! it arrives runs dry many times a second, and every gap is heard as a
//! crackle. This buffer waits until a small reserve has collected before it
//! starts, keeps playing from the reserve while packets are late, and when
//! it does run dry it fades out and back in instead of cutting the sound.
//!
//! It is not a full jitter buffer: packets are played in arrival order, and
//! a lost packet is a short silence, not concealed speech.

use std::collections::VecDeque;

/// Reserve collected before playback starts, in milliseconds.
pub const TARGET_MS: u32 = 80;
/// Above this much queued audio the oldest part is dropped, so a burst
/// cannot leave the call running with a long delay.
pub const MAX_MS: u32 = 300;
/// Length of the fade at the edges of a gap.
const FADE_MS: u32 = 5;

#[derive(Debug)]
pub struct PlayoutBuffer {
    queue: VecDeque<f32>,
    target: usize,
    max: usize,
    fade: usize,
    playing: bool,
    /// Samples of the fade-in still to play.
    fade_in_left: usize,
    underruns: u64,
    dropped: u64,
}

impl PlayoutBuffer {
    /// `sample_rate` is the rate of the samples pushed and pulled.
    pub fn new(sample_rate: u32) -> Self {
        let samples = |milliseconds: u32| {
            (u64::from(sample_rate) * u64::from(milliseconds) / 1_000).max(1) as usize
        };
        Self {
            queue: VecDeque::new(),
            target: samples(TARGET_MS),
            max: samples(MAX_MS),
            fade: samples(FADE_MS),
            playing: false,
            fade_in_left: 0,
            underruns: 0,
            dropped: 0,
        }
    }

    /// Adds received audio.
    pub fn push(&mut self, samples: &[f32]) {
        self.queue.extend(samples.iter().copied());
        if self.queue.len() > self.max {
            // Too far behind real time: skip ahead to the normal reserve.
            let excess = self.queue.len() - self.target;
            self.queue.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Fills `output` for the sound card: audio when there is a reserve,
    /// silence otherwise.
    pub fn pull(&mut self, output: &mut [f32]) {
        if !self.playing {
            if self.queue.len() < self.target {
                output.fill(0.0);
                return;
            }
            self.playing = true;
            self.fade_in_left = self.fade;
        }

        // The reserve ends with this block (inside it or exactly at its end):
        // fade its tail to silence and collect a new reserve afterwards.
        // Deciding now, while the tail is still here, is what avoids a click.
        let running_dry = self.queue.len() <= output.len();
        if running_dry {
            let length = self.queue.len();
            let fade = self.fade.min(length);
            for (step, sample) in self.queue.iter_mut().skip(length - fade).enumerate() {
                *sample *= (fade - step - 1) as f32 / fade as f32;
            }
        }

        for slot in output.iter_mut() {
            let mut sample = self.queue.pop_front().unwrap_or(0.0);
            if self.fade_in_left > 0 {
                sample *= (self.fade - self.fade_in_left) as f32 / self.fade as f32;
                self.fade_in_left -= 1;
            }
            *slot = sample;
        }

        if running_dry {
            self.playing = false;
            self.fade_in_left = 0;
            self.underruns += 1;
        }
    }

    /// Samples waiting to be played.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// How many times playback ran out of audio.
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Samples skipped because too much audio had queued up.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 8_000;
    const PACKET: usize = 160; // 20 ms
    const TARGET: usize = 640; // 80 ms
    const FADE: usize = 40; // 5 ms

    fn pull(buffer: &mut PlayoutBuffer, samples: usize) -> Vec<f32> {
        let mut output = vec![9.0; samples];
        buffer.pull(&mut output);
        output
    }

    #[test]
    fn silence_until_the_reserve_has_collected() {
        let mut buffer = PlayoutBuffer::new(RATE);
        for _ in 0..3 {
            buffer.push(&[1.0; PACKET]);
            assert_eq!(pull(&mut buffer, PACKET), vec![0.0; PACKET]);
        }
        // Nothing was consumed while waiting.
        assert_eq!(buffer.queued(), 3 * PACKET);

        buffer.push(&[1.0; PACKET]);
        assert_eq!(buffer.queued(), TARGET);
        let first = pull(&mut buffer, PACKET);
        assert!(first.iter().any(|sample| *sample > 0.0));
        assert_eq!(buffer.underruns(), 0);
    }

    #[test]
    fn playback_starts_with_a_fade_in_and_then_plays_unchanged() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[1.0; TARGET]);
        let output = pull(&mut buffer, PACKET);
        assert_eq!(output[0], 0.0);
        for pair in output[..FADE].windows(2) {
            assert!(pair[1] > pair[0]);
        }
        assert!(output[FADE..].iter().all(|sample| *sample == 1.0));
    }

    #[test]
    fn late_packets_are_bridged_by_the_reserve() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[0.5; TARGET]);
        pull(&mut buffer, PACKET);

        // Two packet times pass with nothing arriving, then a burst of three.
        for _ in 0..2 {
            let output = pull(&mut buffer, PACKET);
            assert!(output.iter().all(|sample| *sample == 0.5));
        }
        buffer.push(&[0.5; 3 * PACKET]);
        for _ in 0..3 {
            let output = pull(&mut buffer, PACKET);
            assert!(output.iter().all(|sample| *sample == 0.5));
        }
        assert_eq!(buffer.underruns(), 0);
    }

    #[test]
    fn running_dry_fades_out_then_waits_for_a_new_reserve() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[1.0; TARGET]);
        pull(&mut buffer, TARGET - 100);

        // Only 100 samples are left for a 160-sample block.
        let output = pull(&mut buffer, PACKET);
        assert!(output[..100 - FADE].iter().all(|sample| *sample == 1.0));
        for pair in output[100 - FADE..100].windows(2) {
            assert!(pair[1] < pair[0]);
        }
        assert_eq!(output[99], 0.0);
        assert!(output[100..].iter().all(|sample| *sample == 0.0));
        assert_eq!(buffer.underruns(), 1);

        // One packet is not a reserve yet: still silence, nothing consumed.
        buffer.push(&[1.0; PACKET]);
        assert_eq!(pull(&mut buffer, PACKET), vec![0.0; PACKET]);
        assert_eq!(buffer.queued(), PACKET);

        buffer.push(&[1.0; TARGET - PACKET]);
        let output = pull(&mut buffer, PACKET);
        assert_eq!(output[0], 0.0);
        assert_eq!(output[PACKET - 1], 1.0);
        assert_eq!(buffer.underruns(), 1);
    }

    #[test]
    fn reserve_that_ends_exactly_with_a_block_is_faded_too() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[1.0; TARGET]);
        pull(&mut buffer, TARGET - PACKET);

        // Exactly one block is left. Without the fade the next block would
        // start with a jump from full level to silence.
        let output = pull(&mut buffer, PACKET);
        assert!(output[..PACKET - FADE].iter().all(|sample| *sample == 1.0));
        assert_eq!(output[PACKET - 1], 0.0);
        assert_eq!(buffer.underruns(), 1);

        // Audio that arrives now waits for a full reserve again.
        buffer.push(&[1.0; PACKET]);
        assert_eq!(pull(&mut buffer, PACKET), vec![0.0; PACKET]);
    }

    #[test]
    fn a_large_burst_is_cut_back_to_the_reserve() {
        let mut buffer = PlayoutBuffer::new(RATE);
        let max = 2_400; // 300 ms
        let burst: Vec<f32> = (0..max + 1).map(|index| index as f32).collect();
        buffer.push(&burst);
        assert_eq!(buffer.queued(), TARGET);
        assert_eq!(buffer.dropped(), (max + 1 - TARGET) as u64);

        // The newest audio is what remains: the kept part starts at this
        // input sample. Skip the faded start, and stop before the end so the
        // reserve does not run dry in this pull.
        let first_kept = max + 1 - TARGET;
        let length = TARGET - PACKET;
        let output = pull(&mut buffer, length);
        assert_eq!(output[FADE], (first_kept + FADE) as f32);
        assert_eq!(output[length - 1], (first_kept + length - 1) as f32);
    }

    #[test]
    fn exactly_the_limit_is_kept_whole() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[0.0; 2_400]);
        assert_eq!(buffer.queued(), 2_400);
        assert_eq!(buffer.dropped(), 0);
    }
}
