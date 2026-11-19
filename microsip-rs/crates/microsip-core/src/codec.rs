//! Voice codecs the phone can use, and choosing one with the other side.
//!
//! G.711 (PCMU, PCMA) is done here: one byte per sample at 8 kHz. G.722
//! (16 kHz in the same 64 kbit/s), Opus and G.729 (8 kbit/s) come from the
//! `audio-codec` library. Those three keep state from packet to packet, so
//! a call has one [`VoiceEncoder`] and one [`VoiceDecoder`] per codec.
//! GSM 06.10 (13 kbit/s) and Speex (narrowband and 16 kHz wideband) come
//! from the `oxideav` libraries; they too keep state between packets.

use crate::rtp::{decode_pcmu, encode_pcmu};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Codec {
    #[default]
    Pcmu,
    Pcma,
    G722,
    Opus,
    G729,
    Gsm,
    Speex,
    SpeexWide,
}

/// Bit rate the Opus encoder aims at: clear wideband speech.
const OPUS_BITRATE: i32 = 24_000;

/// Speex quality, 0 to 10. 8 is the usual default of Speex phones:
/// 15 kbit/s narrowband, about 28 kbit/s wideband.
const SPEEX_QUALITY: u8 = 8;

/// Bytes of one GSM 06.10 frame (20 ms) as RTP carries it.
const GSM_FRAME_BYTES: usize = 33;

impl Codec {
    /// The codecs a new installation has switched on, in its order. G.711
    /// comes first: every telephone exchange has it and it costs nothing
    /// to compute. GSM and Speex are there for the exchanges that need
    /// them; the user switches them on, as in MicroSIP.
    pub const DEFAULT: [Codec; 5] = [
        Codec::Pcmu,
        Codec::Pcma,
        Codec::G722,
        Codec::Opus,
        Codec::G729,
    ];

    /// Every codec the phone has, in the order the list shows them.
    pub const PREFERRED: [Codec; 8] = [
        Codec::Pcmu,
        Codec::Pcma,
        Codec::G722,
        Codec::Opus,
        Codec::G729,
        Codec::Gsm,
        Codec::Speex,
        Codec::SpeexWide,
    ];

    /// The RTP payload type this phone offers the codec with. For Opus it
    /// and Speex it is only a proposal: the type agreed in SDP is what is
    /// used.
    pub const fn payload_type(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Gsm => 3,
            Codec::Pcma => 8,
            Codec::G722 => 9,
            Codec::G729 => 18,
            Codec::Speex => 97,
            Codec::SpeexWide => 98,
            Codec::Opus => 111,
        }
    }

    /// True when the payload type is fixed by the standard, so that the
    /// number alone names the codec.
    pub const fn has_fixed_payload_type(self) -> bool {
        !matches!(self, Codec::Opus | Codec::Speex | Codec::SpeexWide)
    }

    /// Name and clock rate as written in SDP.
    pub const fn rtpmap(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU/8000",
            Codec::Pcma => "PCMA/8000",
            // The standard says 8000 for G.722 although it carries 16 kHz.
            Codec::G722 => "G722/8000",
            Codec::G729 => "G729/8000",
            Codec::Gsm => "GSM/8000",
            Codec::Speex => "speex/8000",
            Codec::SpeexWide => "speex/16000",
            // Opus is always announced this way, also for mono speech.
            Codec::Opus => "opus/48000/2",
        }
    }

    /// Parameters for the SDP `a=fmtp` line, if the codec has any.
    pub const fn fmtp(self) -> Option<&'static str> {
        match self {
            // This phone neither sends nor expects G.729 silence frames.
            Codec::G729 => Some("annexb=no"),
            Codec::Opus => {
                Some("useinbandfec=1; maxplaybackrate=16000; sprop-maxcapturerate=16000")
            }
            _ => None,
        }
    }

    /// Rate of the sound the codec takes and gives, in samples per second.
    pub const fn sample_rate(self) -> u32 {
        match self {
            Codec::Pcmu | Codec::Pcma | Codec::G729 | Codec::Gsm | Codec::Speex => 8_000,
            Codec::G722 | Codec::Opus | Codec::SpeexWide => 16_000,
        }
    }

    /// Rate of the RTP timestamps, which is not always the sound's rate.
    pub const fn clock_rate(self) -> u32 {
        match self {
            Codec::Opus => 48_000,
            Codec::SpeexWide => 16_000,
            _ => 8_000,
        }
    }

    /// Samples in one 20 ms packet.
    pub const fn frame_samples(self) -> usize {
        self.sample_rate() as usize / 50
    }

    /// Short name, as kept in the settings file and typed in commands.
    pub const fn name(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU",
            Codec::Pcma => "PCMA",
            Codec::G722 => "G722",
            Codec::G729 => "G729",
            Codec::Opus => "opus",
            Codec::Gsm => "GSM",
            Codec::Speex => "speex",
            Codec::SpeexWide => "speex16",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::PREFERRED
            .into_iter()
            .find(|codec| codec.name().eq_ignore_ascii_case(name))
            .or_else(|| match name.to_ascii_lowercase().as_str() {
                "ulaw" | "g711u" => Some(Codec::Pcmu),
                "alaw" | "g711a" => Some(Codec::Pcma),
                "speex8" | "speex/8000" => Some(Codec::Speex),
                "speex-wb" | "speexwb" | "speex/16000" => Some(Codec::SpeexWide),
                _ => None,
            })
    }

    /// The codec an SDP `a=rtpmap` encoding (`G722/8000`) names.
    pub fn from_rtpmap(encoding: &str) -> Option<Self> {
        let mut parts = encoding.trim().split('/');
        let name = parts.next()?;
        let rate = parts.next()?;
        Self::PREFERRED.into_iter().find(|codec| {
            let mut own = codec.rtpmap().split('/');
            own.next().is_some_and(|own| own.eq_ignore_ascii_case(name)) && own.next() == Some(rate)
        })
    }

    /// The codec of a payload type whose number is fixed by the standard.
    pub fn from_payload_type(payload_type: u8) -> Option<Self> {
        Self::PREFERRED
            .into_iter()
            .find(|codec| codec.has_fixed_payload_type() && codec.payload_type() == payload_type)
    }

    /// Reads a list such as `PCMA, G722, opus`. Unknown names and repeats
    /// are dropped; an empty result is the default list, so the phone is
    /// never left without a codec.
    pub fn parse_list(text: &str) -> Vec<Self> {
        let mut list = Vec::new();
        for codec in text.split([',', ' ', ';']).filter_map(Self::from_name) {
            if !list.contains(&codec) {
                list.push(codec);
            }
        }
        if list.is_empty() {
            list = Self::DEFAULT.to_vec();
        }
        list
    }

    pub fn format_list(list: &[Self]) -> String {
        let names: Vec<&str> = list.iter().map(|codec| codec.name()).collect();
        names.join(",")
    }

    /// Encodes one stretch of sound without keeping anything for the next.
    /// Right for G.711; for the other codecs use a [`VoiceEncoder`].
    pub fn encode(self, samples: &[f32]) -> Vec<u8> {
        VoiceEncoder::new(self).encode(samples)
    }

    /// Decodes without keeping state; see [`Codec::encode`].
    pub fn decode(self, payload: &[u8]) -> Vec<f32> {
        VoiceDecoder::new(self).decode(payload)
    }

    /// What the audio part of the other side's SDP lists: payload types in
    /// their order, each with the codec it means for this phone. `rtpmaps`
    /// are the `a=rtpmap` values (`111 opus/48000/2`). A type this phone
    /// has no codec for is left out.
    pub fn listed(formats: &[String], rtpmaps: &[&str]) -> Vec<(u8, Self)> {
        formats
            .iter()
            .filter_map(|format| format.trim().parse::<u8>().ok())
            .filter_map(|payload_type| {
                let named = rtpmaps.iter().find_map(|rtpmap| {
                    let (number, encoding) = rtpmap.trim().split_once(' ')?;
                    (number.parse::<u8>().ok()? == payload_type).then_some(encoding)
                });
                let codec = match named {
                    Some(encoding) => Self::from_rtpmap(encoding),
                    None => Self::from_payload_type(payload_type),
                }?;
                Some((payload_type, codec))
            })
            .collect()
    }

    /// The codec to use with the other side, and its payload type there.
    /// `listed` is what they list (see [`Codec::listed`]), `enabled` what
    /// this phone allows, in the user's order. For an offer the phone's own
    /// order decides among what is offered; in an answer the first usable
    /// entry is what the other side chose.
    pub fn choose(listed: &[(u8, Self)], enabled: &[Self], is_offer: bool) -> Option<(Self, u8)> {
        let found = if is_offer {
            enabled
                .iter()
                .find_map(|ours| listed.iter().find(|(_, codec)| codec == ours))
        } else {
            listed.first()
        };
        found.map(|(payload_type, codec)| (*codec, *payload_type))
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Codec::Pcmu => "G.711u",
            Codec::Pcma => "G.711a",
            Codec::G722 => "G.722",
            Codec::G729 => "G.729",
            Codec::Opus => "Opus",
            Codec::Gsm => "GSM",
            Codec::Speex => "Speex",
            Codec::SpeexWide => "Speex 16 кГц",
        })
    }
}

fn to_pcm(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
        .collect()
}

fn from_pcm(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect()
}

/// The encoder behind a [`VoiceEncoder`]; G.711 needs none.
enum EncoderEngine {
    Plain,
    Library(Box<dyn audio_codec::Encoder>),
    Gsm(Box<oxideav_gsm::EncoderState>),
    Speex(Box<oxideav_speex::NarrowbandEncoder>),
    SpeexWide(Box<oxideav_speex::WidebandEncoder>),
}

/// Turns the sound of one call into packets of one codec.
pub struct VoiceEncoder {
    codec: Codec,
    engine: EncoderEngine,
}

impl VoiceEncoder {
    pub fn new(codec: Codec) -> Self {
        let engine = match codec {
            Codec::Pcmu | Codec::Pcma => EncoderEngine::Plain,
            Codec::G722 => EncoderEngine::Library(Box::new(audio_codec::g722::G722Encoder::new())),
            Codec::G729 => EncoderEngine::Library(Box::new(audio_codec::g729::G729Encoder::new())),
            Codec::Gsm => EncoderEngine::Gsm(Box::new(oxideav_gsm::EncoderState::new())),
            Codec::Speex => EncoderEngine::Speex(Box::new(oxideav_speex::NarrowbandEncoder::new())),
            Codec::SpeexWide => {
                EncoderEngine::SpeexWide(Box::new(oxideav_speex::WidebandEncoder::new()))
            }
            Codec::Opus => {
                let mut encoder = audio_codec::opus::OpusEncoder::new(codec.sample_rate(), 1);
                encoder.set_bitrate(OPUS_BITRATE);
                encoder.set_cbr(false);
                // A spare copy of each packet rides in the next one, for
                // networks that lose a few percent.
                encoder.set_inband_fec(true);
                encoder.set_packet_loss_perc(5);
                EncoderEngine::Library(Box::new(encoder))
            }
        };
        Self { codec, engine }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Encodes samples in `-1.0..=1.0` at the codec's sample rate; one
    /// packet's worth ([`Codec::frame_samples`]) per call.
    pub fn encode(&mut self, samples: &[f32]) -> Vec<u8> {
        match (self.codec, &mut self.engine) {
            (Codec::Pcmu, _) => encode_pcmu(samples),
            (Codec::Pcma, _) => encode_pcma(samples),
            (_, EncoderEngine::Library(library)) => library.encode(&to_pcm(samples)),
            (_, EncoderEngine::Gsm(encoder)) => {
                let mut packet = Vec::new();
                for frame in frames::<160>(samples) {
                    let frame = encoder.encode_frame(&frame);
                    packet.extend_from_slice(&frame.to_gsm_byte_frame());
                }
                packet
            }
            (_, EncoderEngine::Speex(encoder)) => {
                let frames = frames::<160>(samples);
                encoder
                    .encode_packet_quality(&frames, SPEEX_QUALITY)
                    .unwrap_or_default()
            }
            (_, EncoderEngine::SpeexWide(encoder)) => {
                let frames = frames::<320>(samples);
                encoder
                    .encode_packet_quality(&frames, SPEEX_QUALITY)
                    .unwrap_or_default()
            }
            (_, EncoderEngine::Plain) => Vec::new(),
        }
    }
}

impl fmt::Debug for VoiceEncoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "VoiceEncoder({:?})", self.codec)
    }
}

/// Splits sound into whole frames of `N` samples for the codecs that work
/// frame by frame; a short rest is filled with silence.
fn frames<const N: usize>(samples: &[f32]) -> Vec<[i16; N]> {
    to_pcm(samples)
        .chunks(N)
        .map(|chunk| {
            let mut frame = [0_i16; N];
            frame[..chunk.len()].copy_from_slice(chunk);
            frame
        })
        .collect()
}

/// Loudest sample a Speex frame may come out with before the decoder is
/// taken to have run away. Real speech stays within the 16-bit range, give
/// or take a little overshoot.
///
/// The Speex library computes in 64-bit floating point while the encoders
/// of other phones use 32 bits. On a long, loud, steady tone (music on
/// hold, a test tone) the tiny difference can grow from packet to packet
/// until the sound is nothing but full-scale noise. The reference decoder
/// limits its excitation; the library does not let that be done from
/// outside, so a decoder that has run away is replaced by a fresh one:
/// 20 ms of silence instead of noise for the rest of the call.
const SPEEX_RUNAWAY_PEAK: f64 = 1.1 * 32_768.0;

/// The decoder behind a [`VoiceDecoder`]; G.711 needs none.
enum DecoderEngine {
    Plain,
    Library(Box<dyn audio_codec::Decoder>),
    Gsm(Box<oxideav_gsm::DecoderState>),
    Speex(Box<oxideav_speex::SpeexDecoder>),
}

/// Turns received packets of one codec back into sound.
pub struct VoiceDecoder {
    codec: Codec,
    engine: DecoderEngine,
    /// The library's decoder fills lost packets by itself.
    conceals: bool,
}

impl VoiceDecoder {
    pub fn new(codec: Codec) -> Self {
        let mut engine = match codec {
            Codec::Pcmu | Codec::Pcma => DecoderEngine::Plain,
            Codec::G722 => DecoderEngine::Library(Box::new(audio_codec::g722::G722Decoder::new())),
            Codec::G729 => DecoderEngine::Library(Box::new(audio_codec::g729::G729Decoder::new())),
            Codec::Opus => DecoderEngine::Library(Box::new(audio_codec::opus::OpusDecoder::new(
                codec.sample_rate(),
                1,
            ))),
            Codec::Gsm => DecoderEngine::Gsm(Box::new(oxideav_gsm::DecoderState::new())),
            Codec::Speex | Codec::SpeexWide => DecoderEngine::Speex(new_speex_decoder()),
        };
        let conceals = match &mut engine {
            DecoderEngine::Library(library) => library.enable_concealment(),
            _ => false,
        };
        Self {
            codec,
            engine,
            conceals,
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Sound of one packet, at the codec's sample rate. Bytes that are not
    /// a packet of this codec give no sound.
    pub fn decode(&mut self, payload: &[u8]) -> Vec<f32> {
        match (self.codec, &mut self.engine) {
            (Codec::Pcmu, _) => decode_pcmu(payload),
            (Codec::Pcma, _) => decode_pcma(payload),
            (_, DecoderEngine::Library(library)) => from_pcm(&library.decode(payload)),
            (_, DecoderEngine::Gsm(decoder)) => {
                // A packet may carry several frames; a broken one ends it.
                let mut sound = Vec::new();
                for bytes in payload.as_chunks::<GSM_FRAME_BYTES>().0 {
                    let Ok(frame) = oxideav_gsm::UnpackedFrame::from_gsm_byte_frame(bytes) else {
                        break;
                    };
                    sound.extend(from_pcm(&decoder.decode_frame(&frame)));
                }
                sound
            }
            (codec, DecoderEngine::Speex(decoder)) => {
                let frames = decoder.decode_packet(payload).unwrap_or_default();
                let mut sound = Vec::new();
                for frame in frames {
                    // Only frames of the agreed band are sound for this call.
                    if frame.sample_rate_hz() != Some(codec.sample_rate()) {
                        continue;
                    }
                    let Some(samples) = speex_samples(&frame) else {
                        continue;
                    };
                    let peak = samples
                        .iter()
                        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
                    // NaN counts as running away too.
                    if peak.is_nan() || peak > SPEEX_RUNAWAY_PEAK {
                        *decoder = new_speex_decoder();
                        sound.extend(std::iter::repeat_n(0.0, samples.len()));
                        continue;
                    }
                    sound.extend(
                        samples
                            .iter()
                            .map(|sample| (sample / 32_768.0).clamp(-1.0, 1.0) as f32),
                    );
                }
                sound
            }
            (_, DecoderEngine::Plain) => Vec::new(),
        }
    }

    /// Sound to play in place of a lost packet of `samples` samples, when
    /// the codec can make it up from what came before; `None` otherwise.
    pub fn conceal(&mut self, samples: usize) -> Option<Vec<f32>> {
        if !self.conceals {
            return None;
        }
        let DecoderEngine::Library(library) = &mut self.engine else {
            return None;
        };
        let made = library.conceal(samples);
        (!made.is_empty()).then(|| from_pcm(&made))
    }
}

fn new_speex_decoder() -> Box<oxideav_speex::SpeexDecoder> {
    let mut decoder = oxideav_speex::SpeexDecoder::new();
    // The perceptual enhancer that Speex phones switch on.
    decoder.set_enhancer(true);
    Box::new(decoder)
}

/// The sound of a decoded Speex frame before rounding, so that a decoder
/// running away shows as samples far outside the 16-bit range.
fn speex_samples(frame: &oxideav_speex::DecodedFrame) -> Option<&[f64]> {
    match frame {
        oxideav_speex::DecodedFrame::Narrowband(samples) => Some(&samples[..]),
        oxideav_speex::DecodedFrame::Wideband { wideband_pcm, .. } => Some(&wideband_pcm[..]),
        _ => None,
    }
}

impl fmt::Debug for VoiceDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "VoiceDecoder({:?})", self.codec)
    }
}

/// Upper end of each A-law segment, for 13-bit magnitudes.
const SEGMENT_ENDS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

fn linear_to_alaw(pcm: i32) -> u8 {
    // 16-bit input is reduced to the 13 bits A-law works with.
    let mut value = pcm >> 3;
    let mask = if value >= 0 {
        0xD5
    } else {
        value = -value - 1;
        0x55
    };
    let Some(segment) = SEGMENT_ENDS.iter().position(|end| value <= *end) else {
        return 0x7F ^ mask;
    };
    let shift = if segment < 2 { 1 } else { segment };
    let code = ((segment as i32) << 4) | ((value >> shift) & 0x0F);
    (code as u8) ^ mask
}

fn alaw_to_linear(byte: u8) -> i32 {
    let value = byte ^ 0x55;
    let mantissa = i32::from(value & 0x0F) << 4;
    let segment = i32::from((value & 0x70) >> 4);
    let magnitude = match segment {
        0 => mantissa + 8,
        1 => mantissa + 0x108,
        _ => (mantissa + 0x108) << (segment - 1),
    };
    if value & 0x80 != 0 {
        magnitude
    } else {
        -magnitude
    }
}

pub fn encode_pcma(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .map(|sample| linear_to_alaw((sample.clamp(-1.0, 1.0) * 32_767.0).round() as i32))
        .collect()
}

pub fn decode_pcma(payload: &[u8]) -> Vec<f32> {
    payload
        .iter()
        .map(|byte| alaw_to_linear(*byte) as f32 / 32_768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alaw_matches_the_g711_reference_values() {
        // Values of the ITU-T G.711 A-law tables (16-bit linear, code byte).
        for (linear, code) in [
            (0, 0xD5),
            (8, 0xD5),
            (16, 0xD4),
            (-8, 0x55),
            (-1, 0x55),
            (1_000, 0xFA),
            (-1_000, 0x7A),
            (32_767, 0xAA),
            (-32_768, 0x2A),
        ] {
            assert_eq!(linear_to_alaw(linear), code, "encode {linear}");
        }
        for (code, linear) in [
            (0xD5, 8),
            (0x55, -8),
            (0xD4, 24),
            (0xAA, 32_256),
            (0x2A, -32_256),
            (0xFA, 1_008),
        ] {
            assert_eq!(alaw_to_linear(code), linear, "decode {code:#x}");
        }
    }

    #[test]
    fn every_alaw_code_survives_decoding_and_encoding() {
        for code in 0..=255_u8 {
            assert_eq!(linear_to_alaw(alaw_to_linear(code)), code, "{code:#x}");
        }
    }

    #[test]
    fn alaw_keeps_speech_within_its_rounding_error() {
        // The error grows with the level: at most about 3 % of the sample
        // plus one small step near zero.
        for index in -2_000..=2_000 {
            let sample = index as f32 / 2_000.0;
            let decoded = decode_pcma(&encode_pcma(&[sample]))[0];
            let allowed = sample.abs() * 0.035 + 0.0006;
            assert!(
                (decoded - sample).abs() <= allowed,
                "{sample} came back as {decoded}"
            );
        }
        // Louder in never means quieter out.
        let ramp: Vec<f32> = (0..=1_000).map(|index| index as f32 / 1_000.0).collect();
        let decoded = decode_pcma(&encode_pcma(&ramp));
        assert!(decoded.windows(2).all(|pair| pair[1] >= pair[0]));
        // Silence stays practically silent.
        assert!(decode_pcma(&encode_pcma(&[0.0]))[0].abs() < 0.0005);
    }

    #[test]
    fn codecs_know_their_sdp_names_and_payload_types() {
        assert_eq!(Codec::Pcmu.payload_type(), 0);
        assert_eq!(Codec::Pcma.payload_type(), 8);
        assert_eq!(Codec::G722.payload_type(), 9);
        assert_eq!(Codec::G729.payload_type(), 18);
        assert_eq!(Codec::Pcmu.rtpmap(), "PCMU/8000");
        assert_eq!(Codec::Pcma.rtpmap(), "PCMA/8000");
        assert_eq!(Codec::G722.rtpmap(), "G722/8000");
        assert_eq!(Codec::Opus.rtpmap(), "opus/48000/2");
        assert_eq!(Codec::from_payload_type(0), Some(Codec::Pcmu));
        assert_eq!(Codec::from_payload_type(8), Some(Codec::Pcma));
        assert_eq!(Codec::from_payload_type(9), Some(Codec::G722));
        assert_eq!(Codec::from_payload_type(18), Some(Codec::G729));
        // Opus has no number of its own; 111 is only what this phone offers.
        assert_eq!(Codec::from_payload_type(111), None);
        assert_eq!(Codec::from_payload_type(101), None);
        assert_eq!(Codec::default(), Codec::Pcmu);
        assert_eq!(Codec::from_rtpmap("OPUS/48000/2"), Some(Codec::Opus));
        assert_eq!(Codec::from_rtpmap("g729/8000"), Some(Codec::G729));
        assert_eq!(Codec::from_rtpmap("G722/16000"), None);
        assert_eq!(Codec::from_rtpmap("telephone-event/8000"), None);
        assert_eq!(Codec::Gsm.payload_type(), 3);
        assert_eq!(Codec::from_payload_type(3), Some(Codec::Gsm));
        assert_eq!(Codec::Gsm.rtpmap(), "GSM/8000");
        assert_eq!(Codec::from_rtpmap("SPEEX/8000"), Some(Codec::Speex));
        assert_eq!(Codec::from_rtpmap("speex/16000"), Some(Codec::SpeexWide));
        // Speex numbers are agreed in SDP, like those of Opus.
        assert!(!Codec::Speex.has_fixed_payload_type());
        assert_eq!(Codec::from_payload_type(97), None);
    }

    #[test]
    fn sound_and_clock_rates_are_those_of_the_standards() {
        // (codec, sound rate, RTP clock, samples per 20 ms)
        for (codec, rate, clock, frame) in [
            (Codec::Pcmu, 8_000, 8_000, 160),
            (Codec::Pcma, 8_000, 8_000, 160),
            (Codec::G722, 16_000, 8_000, 320),
            (Codec::G729, 8_000, 8_000, 160),
            (Codec::Opus, 16_000, 48_000, 320),
            (Codec::Gsm, 8_000, 8_000, 160),
            (Codec::Speex, 8_000, 8_000, 160),
            (Codec::SpeexWide, 16_000, 16_000, 320),
        ] {
            assert_eq!(codec.sample_rate(), rate, "{codec}");
            assert_eq!(codec.clock_rate(), clock, "{codec}");
            assert_eq!(codec.frame_samples(), frame, "{codec}");
        }
    }

    #[test]
    fn codec_lists_are_read_and_written_for_the_settings() {
        assert_eq!(
            Codec::parse_list("PCMA, g722;opus  ulaw"),
            [Codec::Pcma, Codec::G722, Codec::Opus, Codec::Pcmu]
        );
        // Repeats and unknown names are dropped.
        assert_eq!(Codec::parse_list("G729,ilbc,g729"), [Codec::G729]);
        assert_eq!(
            Codec::parse_list("gsm, speex, speex16, speex-wb"),
            [Codec::Gsm, Codec::Speex, Codec::SpeexWide]
        );
        // Nothing usable: the phone keeps all its codecs.
        assert_eq!(Codec::parse_list(""), Codec::DEFAULT);
        assert_eq!(Codec::parse_list("ilbc"), Codec::DEFAULT);
        assert_eq!(Codec::format_list(&[Codec::Opus, Codec::Pcma]), "opus,PCMA");
        for codec in Codec::PREFERRED {
            assert_eq!(Codec::from_name(codec.name()), Some(codec));
        }
        assert_eq!(
            Codec::parse_list(&Codec::format_list(&Codec::PREFERRED)),
            Codec::PREFERRED
        );
    }

    #[test]
    fn each_codec_decodes_its_own_bytes_and_not_the_others() {
        let voice = [0.5_f32, -0.25, 0.1];
        for codec in [Codec::Pcmu, Codec::Pcma] {
            let decoded = codec.decode(&codec.encode(&voice));
            for (sample, back) in voice.iter().zip(&decoded) {
                assert!((sample - back).abs() < 0.03, "{codec:?}");
            }
        }
        // The two laws are different: bytes of one are noise to the other.
        let wrong = Codec::Pcmu.decode(&Codec::Pcma.encode(&voice));
        assert!((wrong[0] - voice[0]).abs() > 0.1);
    }

    /// One second of a 440 Hz tone at `rate`.
    fn tone(rate: u32) -> Vec<f32> {
        (0..rate)
            .map(|index| 0.4 * (std::f32::consts::TAU * 440.0 * index as f32 / rate as f32).sin())
            .collect()
    }

    /// Strength of `frequency` in the samples; a full sine of amplitude
    /// `a` gives about `a`.
    fn strength(samples: &[f32], rate: u32, frequency: f32) -> f32 {
        let (mut real, mut imaginary) = (0.0_f32, 0.0_f32);
        for (index, sample) in samples.iter().enumerate() {
            let angle = std::f32::consts::TAU * frequency * index as f32 / rate as f32;
            real += sample * angle.cos();
            imaginary += sample * angle.sin();
        }
        2.0 * (real * real + imaginary * imaginary).sqrt() / samples.len() as f32
    }

    #[test]
    fn every_codec_carries_a_tone_packet_by_packet() {
        // (codec, bytes of one 20 ms packet; None when the size varies)
        for (codec, packet_bytes) in [
            (Codec::Pcmu, Some(160)),
            (Codec::Pcma, Some(160)),
            (Codec::G722, Some(160)),
            (Codec::G729, Some(20)),
            (Codec::Opus, None),
            (Codec::Gsm, Some(33)),
            // Speex quality 8: 15 kbit/s and about 28 kbit/s.
            (Codec::Speex, Some(39)),
            (Codec::SpeexWide, Some(71)),
        ] {
            let rate = codec.sample_rate();
            let mut encoder = VoiceEncoder::new(codec);
            let mut decoder = VoiceDecoder::new(codec);
            assert_eq!(encoder.codec(), codec);
            assert_eq!(decoder.codec(), codec);
            let mut heard = Vec::new();
            for frame in tone(rate).chunks(codec.frame_samples()) {
                let packet = encoder.encode(frame);
                match packet_bytes {
                    Some(bytes) => assert_eq!(packet.len(), bytes, "{codec}"),
                    // 24 kbit/s is about 60 bytes; never an empty packet.
                    None => assert!((8..=200).contains(&packet.len()), "{codec}"),
                }
                let sound = decoder.decode(&packet);
                assert_eq!(sound.len(), frame.len(), "{codec}");
                heard.extend(sound);
            }
            // After the codec has settled, the tone is there at its level
            // and nothing else of note is.
            let settled = &heard[heard.len() / 2..];
            let level = strength(settled, rate, 440.0);
            assert!((0.3..=0.5).contains(&level), "{codec}: tone at {level}");
            assert!(strength(settled, rate, 1_000.0) < 0.03, "{codec}");
            assert!(settled.iter().all(|sample| sample.abs() <= 1.0), "{codec}");
        }
    }

    #[test]
    fn lost_packets_are_filled_by_the_codecs_that_can() {
        for codec in [Codec::G729, Codec::Opus] {
            let rate = codec.sample_rate();
            let mut encoder = VoiceEncoder::new(codec);
            let mut decoder = VoiceDecoder::new(codec);
            let frames: Vec<Vec<f32>> = tone(rate)
                .chunks(codec.frame_samples())
                .map(<[f32]>::to_vec)
                .collect();
            let mut filled = Vec::new();
            for (index, frame) in frames.iter().enumerate() {
                let packet = encoder.encode(frame);
                if index == 30 {
                    // This packet is lost on the way.
                    filled = decoder.conceal(frame.len()).expect("concealment");
                } else {
                    decoder.decode(&packet);
                }
            }
            assert_eq!(filled.len(), codec.frame_samples(), "{codec}");
            // The gap is filled with the tone that was playing, not silence.
            assert!(strength(&filled, rate, 440.0) > 0.1, "{codec}");
        }
        // G.711 has nothing to make a packet up from: the caller covers it.
        assert_eq!(VoiceDecoder::new(Codec::Pcmu).conceal(160), None);
    }

    #[test]
    fn garbage_is_not_sound() {
        for codec in [
            Codec::G722,
            Codec::G729,
            Codec::Opus,
            Codec::Gsm,
            Codec::Speex,
            Codec::SpeexWide,
        ] {
            let mut decoder = VoiceDecoder::new(codec);
            assert!(decoder.decode(&[]).is_empty(), "{codec}");
            // Odd bytes must not panic, whatever they decode to.
            let sound = decoder.decode(&[0xFF, 0x00, 0x13]);
            assert!(sound.iter().all(|sample| sample.is_finite()), "{codec}");
        }
    }

    #[test]
    fn random_packets_never_break_the_decoders() {
        // Packets from the network can be anything; a decoder that panicked
        // would end the sound of the call.
        let mut seed = 0x2545_F491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for codec in [Codec::Gsm, Codec::Speex, Codec::SpeexWide] {
            let mut decoder = VoiceDecoder::new(codec);
            for _ in 0..2_000 {
                let length = (next() % 120) as usize;
                let packet: Vec<u8> = (0..length).map(|_| next() as u8).collect();
                let sound = decoder.decode(&packet);
                assert!(sound.iter().all(|sample| sample.is_finite()), "{codec}");
                assert!(sound.len() <= 16 * codec.frame_samples(), "{codec}");
            }
        }
    }

    #[test]
    fn what_the_other_side_lists_is_matched_by_name_or_by_number() {
        let formats = |text: &str| -> Vec<String> { text.split(' ').map(str::to_owned).collect() };
        // Without rtpmap lines only the fixed numbers mean something.
        assert_eq!(
            Codec::listed(&formats("9 0 8 18 101 111"), &[]),
            [
                (9, Codec::G722),
                (0, Codec::Pcmu),
                (8, Codec::Pcma),
                (18, Codec::G729)
            ]
        );
        // Dynamic numbers are whatever the other side says they are.
        let rtpmaps = [
            "107 opus/48000/2",
            "0 PCMU/8000",
            "101 telephone-event/8000",
            "96 G722/8000",
        ];
        assert_eq!(
            Codec::listed(&formats("107 0 101 96 97"), &rtpmaps),
            [(107, Codec::Opus), (0, Codec::Pcmu), (96, Codec::G722)]
        );
        // A fixed number given another meaning is not our codec...
        assert_eq!(Codec::listed(&formats("9"), &["9 iLBC/8000"]), []);
        // ...unless the meaning is one of ours. Speex goes by its rate.
        assert_eq!(
            Codec::listed(
                &formats("9 97 98 3"),
                &["9 speex/8000", "97 speex/16000", "98 speex/32000"]
            ),
            [(9, Codec::Speex), (97, Codec::SpeexWide), (3, Codec::Gsm)]
        );
    }

    #[test]
    fn offer_is_answered_by_our_preference_and_answer_by_their_choice() {
        let all = Codec::PREFERRED;
        let theirs = [(8, Codec::Pcma), (0, Codec::Pcmu), (107, Codec::Opus)];
        // Offers: our order decides, whatever order they list.
        assert_eq!(Codec::choose(&theirs, &all, true), Some((Codec::Pcmu, 0)));
        assert_eq!(
            Codec::choose(&theirs, &[Codec::Opus, Codec::Pcmu], true),
            Some((Codec::Opus, 107))
        );
        // A codec the user switched off is not taken from an offer.
        assert_eq!(
            Codec::choose(&theirs, &[Codec::G722, Codec::G729], true),
            None
        );
        assert_eq!(Codec::choose(&[], &all, true), None);
        // Answers: the first codec they list is the one they will send.
        assert_eq!(Codec::choose(&theirs, &all, false), Some((Codec::Pcma, 8)));
        assert_eq!(Codec::choose(&[], &all, false), None);
    }
}
