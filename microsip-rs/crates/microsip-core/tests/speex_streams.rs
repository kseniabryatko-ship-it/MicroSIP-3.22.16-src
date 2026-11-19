//! Speex packets made by another program: eight seconds of a loud, steady
//! 1 kHz tone encoded by the reference Speex library (libspeex, quality 8,
//! one 20 ms frame per packet), each packet kept with its length as two
//! bytes, little end first.
//!
//! A steady tone is the hard case for the Speex library this phone uses:
//! left alone, its decoder runs away after a few seconds and plays nothing
//! but full-scale noise. The phone must notice that and recover.

use microsip_core::codec::{Codec, VoiceDecoder};

const TONE: &[u8] = include_bytes!("data/speex-1khz-tone.bin");

fn packets() -> Vec<&'static [u8]> {
    let mut packets = Vec::new();
    let mut rest = TONE;
    while let [low, high, tail @ ..] = rest {
        let length = usize::from(u16::from_le_bytes([*low, *high]));
        packets.push(&tail[..length]);
        rest = &tail[length..];
    }
    packets
}

/// Strength of `frequency` in the samples; a full sine of amplitude `a`
/// gives about `a`.
fn strength(samples: &[f32], frequency: f32) -> f32 {
    let (mut real, mut imaginary) = (0.0_f32, 0.0_f32);
    for (index, sample) in samples.iter().enumerate() {
        let angle = std::f32::consts::TAU * frequency * index as f32 / 8_000.0;
        real += sample * angle.cos();
        imaginary += sample * angle.sin();
    }
    2.0 * (real * real + imaginary * imaginary).sqrt() / samples.len() as f32
}

#[test]
fn a_long_steady_tone_from_another_phone_stays_a_tone() {
    let packets = packets();
    assert_eq!(packets.len(), 400);
    let mut decoder = VoiceDecoder::new(Codec::Speex);
    let mut sound = Vec::new();
    for packet in &packets {
        let frame = decoder.decode(packet);
        assert_eq!(frame.len(), 160);
        sound.extend(frame);
    }
    // The tone was encoded at amplitude 0.4. Each second of it must still
    // be mostly that tone: never noise, at worst a few short gaps where the
    // decoder was started afresh.
    for (second, samples) in sound.chunks(8_000).enumerate() {
        let level = strength(samples, 1_000.0);
        assert!(
            (0.25..=0.45).contains(&level),
            "second {second}: tone at {level}"
        );
        let clipped = samples
            .iter()
            .filter(|sample| sample.abs() >= 0.999)
            .count();
        assert!(clipped < 80, "second {second}: {clipped} clipped samples");
    }
}
