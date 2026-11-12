//! The whole audio path without sound devices or sockets: microphone samples
//! at 48 kHz are packetized as PCMU, "arrive" unevenly, are decoded, converted
//! back to 48 kHz and played through the playout buffer. The played signal
//! must be a clean tone: no gaps, no clicks at packet seams.

use microsip_core::playout::PlayoutBuffer;
use microsip_core::resample::Resampler;
use microsip_core::rtp::{PCMU_CLOCK_RATE, PcmuStream, decode_pcmu};
use std::collections::VecDeque;
use std::f64::consts::PI;

const DEVICE_RATE: u32 = 48_000;
const BLOCK: usize = 480; // 10 ms at 48 kHz, one sound card callback
const TONE_HZ: f64 = 1_000.0;

/// Plays `blocks` sound card blocks. `arrivals(block)` says how many packets
/// reach the speaker side in that block (the sender makes one per two blocks).
fn play(blocks: usize, arrivals: impl Fn(usize) -> usize) -> (Vec<f32>, PlayoutBuffer) {
    let mut sender = PcmuStream::new(1, DEVICE_RATE).unwrap();
    let mut network = VecDeque::new();
    let mut to_device = Resampler::new(PCMU_CLOCK_RATE, DEVICE_RATE).unwrap();
    let mut playout = PlayoutBuffer::new(DEVICE_RATE);
    let mut played = Vec::new();

    for block in 0..blocks {
        let microphone: Vec<f32> = (0..BLOCK)
            .map(|index| {
                let time = (block * BLOCK + index) as f64 / f64::from(DEVICE_RATE);
                (0.5 * (2.0 * PI * TONE_HZ * time).sin()) as f32
            })
            .collect();
        network.extend(sender.push_pcm(&microphone));

        for _ in 0..arrivals(block) {
            if let Some(packet) = network.pop_front() {
                playout.push(&to_device.push(&decode_pcmu(&packet.payload)));
            }
        }

        let mut output = vec![0.0; BLOCK];
        playout.pull(&mut output);
        played.extend(output);
    }
    (played, playout)
}

fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum();
    (sum / samples.len() as f64).sqrt()
}

/// Largest change between neighbouring samples.
fn largest_jump(samples: &[f32]) -> f64 {
    samples
        .windows(2)
        .map(|pair| f64::from((pair[1] - pair[0]).abs()))
        .fold(0.0, f64::max)
}

/// What a clean tone of this level may change per sample, plus the rounding
/// noise of PCMU (about 2 % of the level at its coarsest step).
fn clean_jump_limit(level: f64) -> f64 {
    level * 2.0 * PI * TONE_HZ / f64::from(DEVICE_RATE) + 0.03 * level
}

#[test]
fn steady_packets_play_as_a_clean_tone() {
    // One packet every second block: exactly as fast as they are made.
    let (played, playout) = play(300, |block| usize::from(block % 2 == 1));
    assert_eq!(playout.underruns(), 0);

    // Skip the start-up reserve and fade-in.
    let steady = &played[20 * BLOCK..];
    let level = rms(steady);
    assert!((level - 0.5 / 2.0_f64.sqrt()).abs() < 0.01, "level {level}");
    let jump = largest_jump(steady);
    assert!(jump <= clean_jump_limit(0.5), "jump {jump}");
}

#[test]
fn bursty_packets_still_play_as_a_clean_tone() {
    // The network holds packets back for 60 ms and then delivers three at
    // once, over and over: far worse than a normal connection.
    let (played, playout) = play(600, |block| if block % 6 == 5 { 3 } else { 0 });
    assert_eq!(
        playout.underruns(),
        0,
        "the reserve should bridge the bursts"
    );
    assert_eq!(playout.dropped(), 0);

    let steady = &played[40 * BLOCK..];
    let level = rms(steady);
    assert!((level - 0.5 / 2.0_f64.sqrt()).abs() < 0.01, "level {level}");
    let jump = largest_jump(steady);
    assert!(jump <= clean_jump_limit(0.5), "jump {jump}");
}

#[test]
fn an_outage_is_a_faded_gap_not_a_click() {
    // Packets stop for 400 ms in the middle of the call, then resume.
    let (played, playout) = play(400, |block| {
        if (150..190).contains(&block) {
            0
        } else {
            usize::from(block % 2 == 1)
        }
    });
    assert_eq!(playout.underruns(), 1);

    // There is real silence during the outage ...
    let silent = &played[175 * BLOCK..185 * BLOCK];
    assert!(silent.iter().all(|sample| *sample == 0.0));
    // ... sound before and after it ...
    assert!(rms(&played[100 * BLOCK..140 * BLOCK]) > 0.3);
    assert!(rms(&played[300 * BLOCK..]) > 0.3);
    // ... and nowhere a jump larger than a clean tone makes, including at
    // the edges of the gap.
    let jump = largest_jump(&played[20 * BLOCK..]);
    assert!(jump <= clean_jump_limit(0.5), "jump {jump}");
}
