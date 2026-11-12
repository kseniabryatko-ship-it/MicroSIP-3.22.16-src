//! Playout buffer between the network and the sound card.
//!
//! Voice packets do not arrive evenly: the network holds some back and then
//! delivers several at once. A sound card that plays each packet the moment
//! it arrives runs dry many times a second, and every gap is heard as a
//! crackle. This buffer waits until a small reserve has collected before it
//! starts, keeps playing from the reserve while packets are late, and when
//! it does run dry it fades out and back in instead of cutting the sound.
//!
//! Packet order and lost packets are handled before this buffer, in
//! `rtp::order`; here only the timing is evened out.
//!
//! An adaptive buffer ([`PlayoutBuffer::adaptive`]) also learns from the
//! connection: every time it runs dry it keeps a larger reserve, up to
//! [`ADAPTIVE_LIMIT_MS`], and after a long quiet stretch it carefully shrinks
//! the reserve again. A bad patch of network then costs one short gap
//! instead of crackling for as long as it lasts.

use std::collections::VecDeque;

/// Reserve collected before playback starts, in milliseconds.
pub const TARGET_MS: u32 = 80;
/// Above this much queued audio the oldest part is dropped, so a burst
/// cannot leave the call running with a long delay.
pub const MAX_MS: u32 = 300;
/// Length of the fade at the edges of a gap.
const FADE_MS: u32 = 5;
/// The largest reserve an adaptive buffer grows to.
pub const ADAPTIVE_LIMIT_MS: u32 = 240;
/// Growth of the reserve after each gap.
const GROW_MS: u32 = 40;
/// Reduction of the reserve after [`CALM_MS`] without a gap.
const SHRINK_MS: u32 = 20;
const CALM_MS: u32 = 30_000;

#[derive(Debug)]
pub struct PlayoutBuffer {
    queue: VecDeque<f32>,
    sample_rate: u32,
    adaptive: bool,
    /// Current reserve in milliseconds; fixed unless adaptive.
    target_ms: u32,
    largest_target_ms: u32,
    /// Samples played since the last gap or reserve change.
    calm: u64,
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
            sample_rate,
            adaptive: false,
            target_ms: TARGET_MS,
            largest_target_ms: TARGET_MS,
            calm: 0,
            target: samples(TARGET_MS),
            max: samples(MAX_MS),
            fade: samples(FADE_MS),
            playing: false,
            fade_in_left: 0,
            underruns: 0,
            dropped: 0,
        }
    }

    /// A buffer that enlarges its reserve when the connection is uneven.
    pub fn adaptive(sample_rate: u32) -> Self {
        Self {
            adaptive: true,
            ..Self::new(sample_rate)
        }
    }

    /// An adaptive buffer that starts with a larger reserve, for example the
    /// one the previous call on the same connection ended with.
    pub fn adaptive_from(sample_rate: u32, start_ms: u32) -> Self {
        let mut buffer = Self::adaptive(sample_rate);
        buffer.set_target(start_ms.clamp(TARGET_MS, ADAPTIVE_LIMIT_MS));
        buffer
    }

    /// The reserve currently collected before playback starts.
    pub fn target_ms(&self) -> u32 {
        self.target_ms
    }

    /// The largest reserve this buffer has needed so far.
    pub fn largest_target_ms(&self) -> u32 {
        self.largest_target_ms
    }

    fn samples(&self, milliseconds: u32) -> usize {
        (u64::from(self.sample_rate) * u64::from(milliseconds) / 1_000).max(1) as usize
    }

    fn set_target(&mut self, milliseconds: u32) {
        self.target_ms = milliseconds;
        self.largest_target_ms = self.largest_target_ms.max(milliseconds);
        self.target = self.samples(milliseconds);
        // The same head-room above the reserve as a fixed buffer has.
        self.max = self.samples(milliseconds + MAX_MS - TARGET_MS);
        self.calm = 0;
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
            if self.adaptive && self.target_ms < ADAPTIVE_LIMIT_MS {
                self.set_target((self.target_ms + GROW_MS).min(ADAPTIVE_LIMIT_MS));
            }
            self.calm = 0;
            return;
        }

        self.calm += output.len() as u64;
        if self.adaptive
            && self.target_ms > TARGET_MS
            && self.calm >= u64::from(self.sample_rate) * u64::from(CALM_MS) / 1_000
        {
            self.set_target((self.target_ms - SHRINK_MS).max(TARGET_MS));
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

    /// Plays until the buffer runs dry once.
    fn run_dry(buffer: &mut PlayoutBuffer) {
        let before = buffer.underruns();
        buffer.push(&vec![0.5; buffer.target_ms() as usize * 8]);
        while buffer.underruns() == before {
            pull(buffer, PACKET);
        }
    }

    #[test]
    fn fixed_buffer_never_changes_its_reserve() {
        let mut buffer = PlayoutBuffer::new(RATE);
        for _ in 0..5 {
            run_dry(&mut buffer);
        }
        assert_eq!(buffer.target_ms(), TARGET_MS);
        assert_eq!(buffer.largest_target_ms(), TARGET_MS);
    }

    #[test]
    fn adaptive_buffer_grows_its_reserve_after_each_gap_up_to_the_limit() {
        let mut buffer = PlayoutBuffer::adaptive(RATE);
        assert_eq!(buffer.target_ms(), 80);
        let mut seen = Vec::new();
        for _ in 0..6 {
            run_dry(&mut buffer);
            seen.push(buffer.target_ms());
        }
        assert_eq!(seen, [120, 160, 200, 240, 240, 240]);

        // The larger reserve is really waited for: 200 ms is not enough now.
        buffer.push(&[0.5; 1_600]);
        assert_eq!(pull(&mut buffer, PACKET), vec![0.0; PACKET]);
        buffer.push(&[0.5; 320]);
        assert!(
            pull(&mut buffer, PACKET)
                .iter()
                .any(|sample| *sample != 0.0)
        );
    }

    #[test]
    fn adaptive_buffer_shrinks_slowly_after_a_long_calm_stretch() {
        let mut buffer = PlayoutBuffer::adaptive(RATE);
        run_dry(&mut buffer);
        run_dry(&mut buffer);
        assert_eq!(buffer.target_ms(), 160);

        // Steady playback: one packet in, one packet out.
        buffer.push(&[0.5; 1_280]);
        let play = |packets: usize, buffer: &mut PlayoutBuffer| {
            for _ in 0..packets {
                buffer.push(&[0.5; PACKET]);
                pull(buffer, PACKET);
            }
        };
        // Just under 30 seconds: nothing changes yet.
        play(1_499, &mut buffer);
        assert_eq!(buffer.target_ms(), 160);
        play(1, &mut buffer);
        assert_eq!(buffer.target_ms(), 140);
        play(1_500, &mut buffer);
        assert_eq!(buffer.target_ms(), 120);
        play(3_000, &mut buffer);
        assert_eq!(buffer.target_ms(), 80);
        play(3_000, &mut buffer);
        assert_eq!(buffer.target_ms(), 80);

        assert_eq!(buffer.underruns(), 2);
        assert_eq!(buffer.largest_target_ms(), 160);
    }

    #[test]
    fn adaptive_buffer_can_start_from_a_remembered_reserve() {
        let mut buffer = PlayoutBuffer::adaptive_from(RATE, 160);
        assert_eq!(buffer.target_ms(), 160);
        // 80 ms is not enough to start now; 160 ms is.
        buffer.push(&[0.5; TARGET]);
        assert_eq!(pull(&mut buffer, PACKET), vec![0.0; PACKET]);
        buffer.push(&[0.5; TARGET]);
        assert!(
            pull(&mut buffer, PACKET)
                .iter()
                .any(|sample| *sample != 0.0)
        );

        // The hint is kept within the range an adaptive buffer uses.
        assert_eq!(PlayoutBuffer::adaptive_from(RATE, 0).target_ms(), 80);
        assert_eq!(PlayoutBuffer::adaptive_from(RATE, 5_000).target_ms(), 240);
    }

    #[test]
    fn exactly_the_limit_is_kept_whole() {
        let mut buffer = PlayoutBuffer::new(RATE);
        buffer.push(&[0.0; 2_400]);
        assert_eq!(buffer.queued(), 2_400);
        assert_eq!(buffer.dropped(), 0);
    }
}
