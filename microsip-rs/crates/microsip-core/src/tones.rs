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

/// A ring melody read from a WAV file, as mono samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Melody {
    samples: Vec<f32>,
    rate: u32,
}

/// The longest file read as a melody.
const MELODY_FILE_LIMIT: u64 = 20 * 1024 * 1024;
/// A longer melody is cut: the ring repeats anyway.
const MELODY_SECONDS_LIMIT: usize = 30;
/// Silence between two rounds of the melody, in milliseconds.
const MELODY_PAUSE_MS: u64 = 1_000;

impl Melody {
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Reads a WAV file: plain PCM of 8, 16, 24 or 32 bits or 32-bit
    /// floating point, any number of channels (they are mixed into one).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let size = std::fs::metadata(path)
            .map_err(|error| error.to_string())?
            .len();
        if size > MELODY_FILE_LIMIT {
            return Err("the file is larger than 20 MB".to_owned());
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        Self::from_wav(&bytes)
    }

    pub fn from_wav(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("not a WAV file".to_owned());
        }
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        // (format, channels, rate, bits)
        let mut format: Option<(u16, usize, u32, usize)> = None;
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let size = u32_at(at + 4) as usize;
            let start = at + 8;
            // A file cut short still plays what is there.
            let end = start.saturating_add(size).min(bytes.len());
            match &bytes[at..at + 4] {
                b"fmt " if end - start >= 16 => {
                    let mut tag = u16_at(start);
                    // "Extensible" keeps the real format further on.
                    if tag == 0xFFFE && end - start >= 26 {
                        tag = u16_at(start + 24);
                    }
                    format = Some((
                        tag,
                        usize::from(u16_at(start + 2)),
                        u32_at(start + 4),
                        usize::from(u16_at(start + 14)),
                    ));
                }
                b"data" => {
                    let (tag, channels, rate, bits) =
                        format.ok_or_else(|| "the WAV file has no format".to_owned())?;
                    return Self::decode(&bytes[start..end], tag, channels, rate, bits);
                }
                _ => {}
            }
            // Chunks are padded to an even size.
            at = start.saturating_add(size).saturating_add(size % 2);
        }
        Err("the WAV file has no sound in it".to_owned())
    }

    fn decode(
        data: &[u8],
        tag: u16,
        channels: usize,
        rate: u32,
        bits: usize,
    ) -> Result<Self, String> {
        let supported = matches!((tag, bits), (1, 8 | 16 | 24 | 32) | (3, 32));
        if !supported || channels == 0 || channels > 8 || !(4_000..=192_000).contains(&rate) {
            return Err(
                "this kind of WAV file is not supported; save it as ordinary PCM".to_owned(),
            );
        }
        let width = bits / 8;
        let one = |sample: &[u8]| -> f32 {
            match (tag, width) {
                (1, 1) => (f32::from(sample[0]) - 128.0) / 128.0,
                (1, 2) => f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0,
                (1, 3) => {
                    i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) as f32
                        / 2_147_483_648.0
                }
                (1, _) => {
                    i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) as f32
                        / 2_147_483_648.0
                }
                _ => f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
            }
        };
        let samples: Vec<f32> = data
            .chunks_exact(width * channels)
            .take(MELODY_SECONDS_LIMIT * rate as usize)
            .map(|frame| {
                let sum: f32 = frame.chunks_exact(width).map(one).sum();
                let mixed = sum / channels as f32;
                if mixed.is_finite() {
                    mixed.clamp(-1.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect();
        if samples.is_empty() {
            return Err("the WAV file has no sound in it".to_owned());
        }
        Ok(Self { samples, rate })
    }
}

/// Plays a [`Melody`] round and round, with a pause between the rounds.
#[derive(Debug)]
pub struct MelodyPlayer {
    melody: std::sync::Arc<Melody>,
    position: u64,
}

impl MelodyPlayer {
    pub fn new(melody: std::sync::Arc<Melody>) -> Self {
        Self {
            melody,
            position: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.melody.rate
    }

    pub fn produced_ms(&self) -> u64 {
        self.position * 1_000 / u64::from(self.melody.rate)
    }

    /// The next `count` samples at the melody's own rate.
    pub fn next_block(&mut self, count: usize) -> Vec<f32> {
        let length = self.melody.samples.len() as u64;
        let cycle = length + u64::from(self.melody.rate) * MELODY_PAUSE_MS / 1_000;
        let block = (0..count as u64)
            .map(|offset| {
                let in_cycle = (self.position + offset) % cycle;
                if in_cycle < length {
                    self.melody.samples[in_cycle as usize]
                } else {
                    0.0
                }
            })
            .collect();
        self.position += count as u64;
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file with the given format and sound bytes.
    fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        // A chunk the reader has to step over, of odd size.
        bytes.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        bytes.extend_from_slice(b"fmt \x10\0\0\0");
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        let block = channels * bits / 8;
        bytes.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        bytes.extend_from_slice(&block.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn melody_reads_16_bit_stereo_as_mono() {
        let mut data = Vec::new();
        for (left, right) in [(16_384_i16, 0_i16), (-16_384, -16_384), (32_767, -32_768)] {
            data.extend_from_slice(&left.to_le_bytes());
            data.extend_from_slice(&right.to_le_bytes());
        }
        let melody = Melody::from_wav(&wav(1, 2, 44_100, 16, &data)).unwrap();
        assert_eq!(melody.rate(), 44_100);
        assert_eq!(melody.samples().len(), 3);
        assert!((melody.samples()[0] - 0.25).abs() < 1e-4);
        assert!((melody.samples()[1] + 0.5).abs() < 1e-4);
        assert!(melody.samples()[2].abs() < 1e-4);
    }

    #[test]
    fn melody_reads_8_bit_24_bit_and_float_sound() {
        let eight = Melody::from_wav(&wav(1, 1, 8_000, 8, &[128, 255, 0])).unwrap();
        assert_eq!(eight.samples()[0], 0.0);
        assert!((eight.samples()[1] - 0.992).abs() < 0.001);
        assert_eq!(eight.samples()[2], -1.0);

        let twenty_four = Melody::from_wav(&wav(1, 1, 48_000, 24, &[0, 0, 0x40, 0, 0, 0xC0]));
        assert_eq!(twenty_four.unwrap().samples(), [0.5, -0.5]);

        let mut data = 0.25_f32.to_le_bytes().to_vec();
        data.extend_from_slice(&f32::NAN.to_le_bytes());
        data.extend_from_slice(&7.0_f32.to_le_bytes());
        let float = Melody::from_wav(&wav(3, 1, 22_050, 32, &data)).unwrap();
        // Sound that is not a number is silence; too loud is cut.
        assert_eq!(float.samples(), [0.25, 0.0, 1.0]);
    }

    #[test]
    fn melody_refuses_what_it_cannot_play() {
        assert!(Melody::from_wav(b"ID3 not a wav file at all").is_err());
        assert!(Melody::from_wav(b"RIFF").is_err());
        // Compressed sound (MP3 inside WAV has tag 0x55).
        assert!(Melody::from_wav(&wav(0x55, 2, 44_100, 16, &[1, 2, 3, 4])).is_err());
        assert!(Melody::from_wav(&wav(1, 0, 44_100, 16, &[1, 2])).is_err());
        assert!(Melody::from_wav(&wav(1, 1, 44_100, 16, &[])).is_err());
        // Sound before any format.
        assert!(Melody::from_wav(b"RIFF\0\0\0\0WAVEdata\x02\0\0\0\x01\x02").is_err());
        // A size that runs past the end of the file is not followed.
        let mut cut = wav(1, 1, 8_000, 16, &[0, 0x40, 0, 0x40]);
        let length = cut.len();
        cut[length - 8..length - 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Melody::from_wav(&cut).unwrap().samples(), [0.5, 0.5]);
    }

    #[test]
    fn melody_file_is_read_from_disk_and_a_missing_one_is_an_error() {
        let directory =
            std::env::temp_dir().join(format!("microsip-rs-melody-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ring.wav");
        assert!(Melody::load(&path).is_err());
        std::fs::write(&path, wav(1, 1, 8_000, 16, &[0, 0x20])).unwrap();
        assert_eq!(Melody::load(&path).unwrap().samples(), [0.25]);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn melody_repeats_after_a_second_of_silence() {
        let melody = Melody {
            samples: vec![0.5; 4_000],
            rate: 8_000,
        };
        let mut player = MelodyPlayer::new(std::sync::Arc::new(melody));
        assert_eq!(player.rate(), 8_000);
        let mut sound = Vec::new();
        for size in [1, 159, 160, 4_001].iter().cycle() {
            if sound.len() >= 24_000 {
                break;
            }
            sound.extend(player.next_block(*size));
        }
        assert_eq!(player.produced_ms(), sound.len() as u64 / 8);
        for (index, sample) in sound.iter().take(24_000).enumerate() {
            let expected = if index % 12_000 < 4_000 { 0.5 } else { 0.0 };
            assert_eq!(*sample, expected, "at sample {index}");
        }
    }

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
