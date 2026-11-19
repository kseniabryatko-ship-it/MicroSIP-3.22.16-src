//! Sending DTMF digits (the tones of a telephone keypad).
//!
//! Two ways, chosen by what the other side agreed to in SDP:
//!
//! * RFC 4733 "telephone-event" packets, when a payload type for them was
//!   negotiated. The digit travels as data, so no codec can distort it.
//! * In-band: the two tones are put into the voice itself. This always works
//!   with G.711 and is the fallback.
//!
//! [`DtmfSender`] is driven once per 20 ms voice frame and says what that
//! frame must carry instead of (or as well as) the microphone.

use std::collections::VecDeque;
use std::f32::consts::PI;

const SAMPLE_RATE: f32 = 8_000.0;
/// Length of a digit: 8 frames of 20 ms.
const TONE_FRAMES: u8 = 8;
/// The final event packet is sent this many times in all (RFC 4733, 2.5.1.4).
const END_PACKETS: u8 = 3;
/// Pause after a digit before the next one may start.
const GAP_FRAMES: u8 = 3;
/// Level of each of the two tones; together they stay well below full scale.
const TONE_LEVEL: f32 = 0.25;
/// Volume field of an event: -10 dBm0, a usual value.
const EVENT_VOLUME: u8 = 10;
/// Digits waiting to be sent; more are refused.
const QUEUE_LIMIT: usize = 64;

/// How the keypad digits of a call are sent, as chosen in the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DtmfMethod {
    /// Telephone-event packets when the other side agreed to them, tones in
    /// the voice otherwise.
    #[default]
    Auto,
    /// Always telephone-event packets (RFC 2833 / 4733), also when the
    /// other side did not announce them.
    Rfc2833,
    /// Always tones in the voice.
    InBand,
    /// SIP INFO requests inside the call (`application/dtmf-relay`), for
    /// exchanges that take digits only this way.
    SipInfo,
}

/// Payload type used for telephone-event when the other side announced
/// none and the user asked for events all the same.
pub const DEFAULT_EVENT_PAYLOAD_TYPE: u8 = 101;

impl DtmfMethod {
    pub const ALL: [DtmfMethod; 4] = [
        DtmfMethod::Auto,
        DtmfMethod::Rfc2833,
        DtmfMethod::InBand,
        DtmfMethod::SipInfo,
    ];

    /// Name in the settings file.
    pub const fn name(self) -> &'static str {
        match self {
            DtmfMethod::Auto => "auto",
            DtmfMethod::Rfc2833 => "rfc2833",
            DtmfMethod::InBand => "inband",
            DtmfMethod::SipInfo => "info",
        }
    }

    /// Unknown names give the automatic choice.
    pub fn from_name(name: &str) -> Self {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|method| method.name().eq_ignore_ascii_case(name))
            .unwrap_or_default()
    }

    /// The telephone-event payload type the audio uses, from the one the
    /// two sides agreed on (if any). `None` means tones in the voice; with
    /// SIP INFO the audio sends no digits at all, so this does not matter.
    pub fn event_payload_type(self, agreed: Option<u8>) -> Option<u8> {
        match self {
            DtmfMethod::Auto | DtmfMethod::SipInfo => agreed,
            DtmfMethod::Rfc2833 => agreed.or(Some(DEFAULT_EVENT_PAYLOAD_TYPE)),
            DtmfMethod::InBand => None,
        }
    }
}

/// Body of a SIP INFO carrying one digit (`application/dtmf-relay`, the
/// form Asterisk, FreeSWITCH and MicroSIP use). Duration in milliseconds.
pub fn info_body(digit: char) -> Option<String> {
    event_code(digit)?;
    Some(format!(
        "Signal={}\r\nDuration=160\r\n",
        digit.to_ascii_uppercase()
    ))
}

/// RFC 4733 event code of a keypad character.
pub fn event_code(digit: char) -> Option<u8> {
    match digit.to_ascii_uppercase() {
        '0'..='9' => Some(digit as u8 - b'0'),
        '*' => Some(10),
        '#' => Some(11),
        letter @ 'A'..='D' => Some(12 + (letter as u8 - b'A')),
        _ => None,
    }
}

/// The sound of a pressed key for the user's own ears (as desk phones and
/// MicroSIP give): its two tones for `duration_ms`, softly faded in and
/// out so it does not click. Empty for a character that is no key.
pub fn key_sound(digit: char, sample_rate: u32, duration_ms: u32) -> Vec<f32> {
    let Some(code) = event_code(digit) else {
        return Vec::new();
    };
    let (low, high) = frequencies(code);
    let length = (sample_rate * duration_ms / 1_000) as usize;
    let fade = (sample_rate as usize / 200).max(1).min(length / 2);
    (0..length)
        .map(|index| {
            let time = index as f32 / sample_rate as f32;
            let edge = index.min(length - 1 - index);
            let envelope = if edge < fade {
                edge as f32 / fade as f32
            } else {
                1.0
            };
            let tones = (2.0 * PI * low * time).sin() + (2.0 * PI * high * time).sin();
            0.12 * envelope * tones
        })
        .collect()
}

/// The low and high tone of a keypad code, in hertz.
fn frequencies(code: u8) -> (f32, f32) {
    const ROWS: [f32; 4] = [697.0, 770.0, 852.0, 941.0];
    const COLUMNS: [f32; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
    // Keypad position of each code: row, column.
    let (row, column) = match code {
        1 => (0, 0),
        2 => (0, 1),
        3 => (0, 2),
        12 => (0, 3),
        4 => (1, 0),
        5 => (1, 1),
        6 => (1, 2),
        13 => (1, 3),
        7 => (2, 0),
        8 => (2, 1),
        9 => (2, 2),
        14 => (2, 3),
        10 => (3, 0),
        0 => (3, 1),
        11 => (3, 2),
        _ => (3, 3),
    };
    (ROWS[row], COLUMNS[column])
}

/// What one 20 ms frame carries while digits are being sent.
#[derive(Debug, Clone, PartialEq)]
pub enum DtmfFrame {
    /// Nothing to send: the frame carries the microphone as usual.
    Voice,
    /// A telephone-event packet replaces the voice packet.
    Event {
        payload_type: u8,
        payload: [u8; 4],
        /// Set on the first packet of a digit.
        marker: bool,
        /// RTP timestamp of the start of the digit, the same for all its packets.
        timestamp: u32,
    },
    /// These samples (at the stream's sound rate) replace the microphone
    /// in the voice packet.
    Tone(Vec<f32>),
    /// Silence replaces the microphone, to separate two in-band digits.
    Silence,
}

#[derive(Debug)]
struct Active {
    code: u8,
    /// Frames of this digit already produced.
    frame: u8,
    start_timestamp: u32,
}

#[derive(Debug)]
pub struct DtmfSender {
    /// Negotiated telephone-event payload type; `None` sends tones in-band.
    payload_type: Option<u8>,
    /// RTP timestamp units in one 20 ms frame of the stream.
    frame_ticks: u16,
    /// Rate of the sound an in-band tone is made at.
    sample_rate: u32,
    queue: VecDeque<u8>,
    active: Option<Active>,
    gap_left: u8,
}

impl Default for DtmfSender {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DtmfSender {
    /// A sender for an 8 kHz stream (G.711).
    pub fn new(payload_type: Option<u8>) -> Self {
        Self::for_stream(payload_type, SAMPLE_RATE as u32, SAMPLE_RATE as u32)
    }

    /// A sender for a stream whose RTP clock runs at `clock_rate` and whose
    /// sound is at `sample_rate`; the two differ for G.722 and Opus.
    pub fn for_stream(payload_type: Option<u8>, clock_rate: u32, sample_rate: u32) -> Self {
        Self {
            payload_type,
            frame_ticks: (clock_rate / 50) as u16,
            sample_rate,
            queue: VecDeque::new(),
            active: None,
            gap_left: 0,
        }
    }

    /// Queues a digit. Returns false for a character that is not on a keypad
    /// or when too many digits are already waiting.
    pub fn queue(&mut self, digit: char) -> bool {
        match event_code(digit) {
            Some(code) if self.queue.len() < QUEUE_LIMIT => {
                self.queue.push_back(code);
                true
            }
            _ => false,
        }
    }

    /// True while a digit is being sent or waiting.
    pub fn is_busy(&self) -> bool {
        self.active.is_some() || !self.queue.is_empty() || self.gap_left > 0
    }

    /// Decides the next frame. `timestamp` is the RTP timestamp of that frame.
    pub fn next_frame(&mut self, timestamp: u32) -> DtmfFrame {
        if self.active.is_none() {
            if self.gap_left > 0 {
                self.gap_left -= 1;
                return match self.payload_type {
                    Some(_) => DtmfFrame::Voice,
                    None => DtmfFrame::Silence,
                };
            }
            let Some(code) = self.queue.pop_front() else {
                return DtmfFrame::Voice;
            };
            self.active = Some(Active {
                code,
                frame: 0,
                start_timestamp: timestamp,
            });
        }
        let Some(active) = self.active.as_mut() else {
            return DtmfFrame::Voice;
        };
        let frame = active.frame;
        active.frame += 1;
        let (code, start_timestamp) = (active.code, active.start_timestamp);

        let produced = match self.payload_type {
            Some(payload_type) => {
                // The duration grows with each packet and stops at the full
                // length, which the end packets repeat.
                let frames = u16::from((frame + 1).min(TONE_FRAMES));
                let duration = frames * self.frame_ticks;
                let end = frame + 1 >= TONE_FRAMES;
                let flags = if end { 0x80 } else { 0 } | EVENT_VOLUME;
                let [high, low] = duration.to_be_bytes();
                DtmfFrame::Event {
                    payload_type,
                    payload: [code, flags, high, low],
                    marker: frame == 0,
                    timestamp: start_timestamp,
                }
            }
            None => DtmfFrame::Tone(tone(code, frame, self.sample_rate)),
        };

        let last = match self.payload_type {
            Some(_) => TONE_FRAMES + END_PACKETS - 1,
            None => TONE_FRAMES,
        };
        if frame + 1 >= last {
            self.active = None;
            self.gap_left = GAP_FRAMES;
        }
        produced
    }
}

/// One frame of the dual tone, continuous in phase from frame to frame and
/// faded in at the start and out at the end of the digit.
fn tone(code: u8, frame: u8, sample_rate: u32) -> Vec<f32> {
    let (low, high) = frequencies(code);
    let rate = sample_rate as f32;
    let frame_samples = (sample_rate / 50) as usize;
    let fade = rate * 0.005; // 5 ms
    let total = f32::from(TONE_FRAMES) * frame_samples as f32;
    (0..frame_samples)
        .map(|index| {
            let position = f32::from(frame) * frame_samples as f32 + index as f32;
            let time = position / rate;
            let envelope = (position / fade)
                .min((total - 1.0 - position) / fade)
                .clamp(0.0, 1.0);
            let sum = (2.0 * PI * low * time).sin() + (2.0 * PI * high * time).sin();
            TONE_LEVEL * envelope * sum
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pressed_key_sounds_its_two_tones_without_clicks() {
        let sound = key_sound('5', 8_000, 120);
        assert_eq!(sound.len(), 960);
        // Soft edges, never loud.
        assert!(sound[0].abs() < 0.01 && sound[959].abs() < 0.05);
        assert!(sound.iter().all(|sample| sample.abs() <= 0.25));
        assert!(sound.iter().any(|sample| sample.abs() > 0.15));
        assert!(key_sound('x', 8_000, 120).is_empty());
    }

    #[test]
    fn the_chosen_method_decides_how_digits_travel() {
        for method in DtmfMethod::ALL {
            assert_eq!(DtmfMethod::from_name(method.name()), method);
        }
        assert_eq!(DtmfMethod::from_name("whatever"), DtmfMethod::Auto);
        assert_eq!(DtmfMethod::Auto.event_payload_type(Some(96)), Some(96));
        assert_eq!(DtmfMethod::Auto.event_payload_type(None), None);
        assert_eq!(DtmfMethod::Rfc2833.event_payload_type(None), Some(101));
        assert_eq!(DtmfMethod::Rfc2833.event_payload_type(Some(96)), Some(96));
        assert_eq!(DtmfMethod::InBand.event_payload_type(Some(96)), None);
        assert_eq!(
            info_body('5').as_deref(),
            Some("Signal=5\r\nDuration=160\r\n")
        );
        assert_eq!(
            info_body('#').as_deref(),
            Some("Signal=#\r\nDuration=160\r\n")
        );
        assert_eq!(
            info_body('a').as_deref(),
            Some("Signal=A\r\nDuration=160\r\n")
        );
        assert_eq!(info_body('x'), None);
    }

    /// Collects frames until the sender is idle again.
    fn frames(sender: &mut DtmfSender, start_timestamp: u32) -> Vec<DtmfFrame> {
        let mut frames = Vec::new();
        let mut timestamp = start_timestamp;
        while sender.is_busy() {
            frames.push(sender.next_frame(timestamp));
            timestamp = timestamp.wrapping_add(160);
            assert!(frames.len() < 1_000, "the sender never finished");
        }
        frames
    }

    /// Strength of one frequency in the samples (Goertzel), normalised so a
    /// full-length sine of amplitude `a` gives about `a`.
    fn strength(samples: &[f32], frequency: f32) -> f32 {
        let omega = 2.0 * PI * frequency / SAMPLE_RATE;
        let coefficient = 2.0 * omega.cos();
        let (mut previous, mut before) = (0.0_f32, 0.0_f32);
        for sample in samples {
            let current = sample + coefficient * previous - before;
            before = previous;
            previous = current;
        }
        let power = previous * previous + before * before - coefficient * previous * before;
        2.0 * power.max(0.0).sqrt() / samples.len() as f32
    }

    #[test]
    fn keypad_characters_have_their_event_codes() {
        let codes: Vec<Option<u8>> = "0123456789*#ABCDd".chars().map(event_code).collect();
        let expected: Vec<Option<u8>> = (0..=15).chain([15]).map(Some).collect();
        assert_eq!(codes, expected);
        assert_eq!(event_code('x'), None);
        assert_eq!(event_code(' '), None);
    }

    #[test]
    fn idle_sender_leaves_the_voice_alone() {
        let mut sender = DtmfSender::new(Some(101));
        assert!(!sender.is_busy());
        assert_eq!(sender.next_frame(0), DtmfFrame::Voice);
        assert!(!sender.queue('x'));
        assert!(!sender.is_busy());
    }

    #[test]
    fn negotiated_digit_is_sent_as_rfc_4733_events() {
        let mut sender = DtmfSender::new(Some(101));
        assert!(sender.queue('5'));
        let frames = frames(&mut sender, 8_000);

        // Eight growing packets, the last of them marked as the end and then
        // repeated twice, followed by a pause carrying voice again.
        assert_eq!(frames.len(), 10 + 3);
        let mut durations = Vec::new();
        for (index, frame) in frames[..10].iter().enumerate() {
            let DtmfFrame::Event {
                payload_type,
                payload,
                marker,
                timestamp,
            } = frame
            else {
                panic!("frame {index} is not an event: {frame:?}");
            };
            assert_eq!(*payload_type, 101);
            assert_eq!(payload[0], 5, "event code");
            assert_eq!(*marker, index == 0);
            // Every packet of the digit carries the timestamp of its start.
            assert_eq!(*timestamp, 8_000);
            assert_eq!(payload[1] & 0x3F, 10, "volume");
            assert_eq!(payload[1] & 0x80 != 0, index >= 7, "end bit at {index}");
            durations.push(u16::from_be_bytes([payload[2], payload[3]]));
        }
        assert_eq!(
            durations,
            [160, 320, 480, 640, 800, 960, 1120, 1280, 1280, 1280]
        );
        assert!(frames[10..].iter().all(|frame| *frame == DtmfFrame::Voice));
    }

    #[test]
    fn digits_follow_each_other_with_a_pause_and_their_own_timestamps() {
        let mut sender = DtmfSender::new(Some(96));
        assert!(sender.queue('1'));
        assert!(sender.queue('#'));
        let frames = frames(&mut sender, 0);
        assert_eq!(frames.len(), 2 * 13);

        let event = |frame: &DtmfFrame| match frame {
            DtmfFrame::Event {
                payload, timestamp, ..
            } => Some((payload[0], *timestamp)),
            _ => None,
        };
        assert_eq!(event(&frames[0]), Some((1, 0)));
        assert_eq!(event(&frames[9]), Some((1, 0)));
        assert_eq!(event(&frames[10]), None);
        // The second digit starts 13 frames after the first.
        assert_eq!(event(&frames[13]), Some((11, 13 * 160)));
    }

    #[test]
    fn without_negotiation_the_digit_is_two_clean_tones_in_the_voice() {
        let mut sender = DtmfSender::new(None);
        assert!(sender.queue('5'));
        let frames = frames(&mut sender, 0);
        assert_eq!(frames.len(), 8 + 3);
        assert!(frames[8..].iter().all(|frame| *frame == DtmfFrame::Silence));

        let samples: Vec<f32> = frames[..8]
            .iter()
            .flat_map(|frame| match frame {
                DtmfFrame::Tone(samples) => samples.clone(),
                other => panic!("expected a tone, got {other:?}"),
            })
            .collect();
        assert_eq!(samples.len(), 1_280);

        // Key 5 is 770 Hz and 1336 Hz; the neighbouring keypad tones are absent.
        assert!((strength(&samples, 770.0) - TONE_LEVEL).abs() < 0.03);
        assert!((strength(&samples, 1_336.0) - TONE_LEVEL).abs() < 0.03);
        for other in [697.0, 852.0, 941.0, 1_209.0, 1_477.0, 1_633.0] {
            assert!(strength(&samples, other) < 0.03, "{other} Hz is present");
        }
        // It starts and ends at silence and never clips.
        assert_eq!(samples[0], 0.0);
        assert!(samples[1_279].abs() < 0.001);
        assert!(
            samples
                .iter()
                .all(|sample| sample.abs() <= 2.0 * TONE_LEVEL)
        );
    }

    #[test]
    fn every_key_has_its_own_pair_of_tones() {
        let mut pairs = Vec::new();
        for digit in "123A456B789C*0#D".chars() {
            let pair = frequencies(event_code(digit).unwrap());
            assert!(!pairs.contains(&pair), "{digit} repeats a pair");
            pairs.push(pair);
        }
        assert_eq!(frequencies(event_code('1').unwrap()), (697.0, 1_209.0));
        assert_eq!(frequencies(event_code('0').unwrap()), (941.0, 1_336.0));
        assert_eq!(frequencies(event_code('#').unwrap()), (941.0, 1_477.0));
        assert_eq!(frequencies(event_code('D').unwrap()), (941.0, 1_633.0));
    }

    #[test]
    fn queue_is_limited() {
        let mut sender = DtmfSender::new(Some(101));
        for _ in 0..QUEUE_LIMIT {
            assert!(sender.queue('1'));
        }
        assert!(!sender.queue('1'));
    }
}
