//! Voice codecs the phone can use, and choosing one with the other side.
//!
//! Both are G.711: one byte per sample at 8 kHz. PCMU (mu-law) is usual in
//! North America and Japan, PCMA (A-law) in Europe and most other places,
//! and many providers accept only one of them.

use crate::rtp::{decode_pcmu, encode_pcmu};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Codec {
    #[default]
    Pcmu,
    Pcma,
}

impl Codec {
    /// Codecs in the order this phone prefers them.
    pub const PREFERRED: [Codec; 2] = [Codec::Pcmu, Codec::Pcma];

    /// The fixed RTP payload type of the codec.
    pub const fn payload_type(self) -> u8 {
        match self {
            Codec::Pcmu => 0,
            Codec::Pcma => 8,
        }
    }

    /// Name and clock rate as written in SDP.
    pub const fn rtpmap(self) -> &'static str {
        match self {
            Codec::Pcmu => "PCMU/8000",
            Codec::Pcma => "PCMA/8000",
        }
    }

    pub fn from_payload_type(payload_type: u8) -> Option<Self> {
        Self::PREFERRED
            .into_iter()
            .find(|codec| codec.payload_type() == payload_type)
    }

    /// Encodes samples in `-1.0..=1.0`, one byte each.
    pub fn encode(self, samples: &[f32]) -> Vec<u8> {
        match self {
            Codec::Pcmu => encode_pcmu(samples),
            Codec::Pcma => encode_pcma(samples),
        }
    }

    pub fn decode(self, payload: &[u8]) -> Vec<f32> {
        match self {
            Codec::Pcmu => decode_pcmu(payload),
            Codec::Pcma => decode_pcma(payload),
        }
    }

    /// The codec to use for the SDP format list of the other side (payload
    /// type numbers as text). For an offer the phone's own preference
    /// decides among what is offered; in an answer the first usable entry is
    /// what the other side chose.
    pub fn choose(formats: &[String], is_offer: bool) -> Option<Self> {
        let offered = |codec: &Codec| {
            formats
                .iter()
                .any(|format| *format == codec.payload_type().to_string())
        };
        if is_offer {
            Self::PREFERRED.into_iter().find(offered)
        } else {
            formats
                .iter()
                .filter_map(|format| format.parse::<u8>().ok())
                .find_map(Self::from_payload_type)
        }
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
        assert_eq!(Codec::Pcmu.rtpmap(), "PCMU/8000");
        assert_eq!(Codec::Pcma.rtpmap(), "PCMA/8000");
        assert_eq!(Codec::from_payload_type(0), Some(Codec::Pcmu));
        assert_eq!(Codec::from_payload_type(8), Some(Codec::Pcma));
        assert_eq!(Codec::from_payload_type(101), None);
        assert_eq!(Codec::default(), Codec::Pcmu);
    }

    #[test]
    fn each_codec_decodes_its_own_bytes_and_not_the_others() {
        let voice = [0.5_f32, -0.25, 0.1];
        for codec in Codec::PREFERRED {
            let decoded = codec.decode(&codec.encode(&voice));
            for (sample, back) in voice.iter().zip(&decoded) {
                assert!((sample - back).abs() < 0.03, "{codec:?}");
            }
        }
        // The two laws are different: bytes of one are noise to the other.
        let wrong = Codec::Pcmu.decode(&Codec::Pcma.encode(&voice));
        assert!((wrong[0] - voice[0]).abs() > 0.1);
    }

    #[test]
    fn offer_is_answered_by_our_preference_and_answer_by_their_choice() {
        let formats = |text: &str| -> Vec<String> { text.split(' ').map(str::to_owned).collect() };
        // Offers: our order decides, whatever order they list.
        assert_eq!(Codec::choose(&formats("8 0 101"), true), Some(Codec::Pcmu));
        assert_eq!(Codec::choose(&formats("8 101"), true), Some(Codec::Pcma));
        assert_eq!(Codec::choose(&formats("18 9 101"), true), None);
        // Answers: the first codec they list is the one they will send.
        assert_eq!(Codec::choose(&formats("8 0 101"), false), Some(Codec::Pcma));
        assert_eq!(Codec::choose(&formats("101 0"), false), Some(Codec::Pcmu));
        assert_eq!(Codec::choose(&formats("18"), false), None);
    }
}
