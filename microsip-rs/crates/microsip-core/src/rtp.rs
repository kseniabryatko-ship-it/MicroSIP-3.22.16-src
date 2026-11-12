pub mod dtmf;
pub mod order;
pub mod session;
use crate::resample::Resampler;
use dtmf::{DtmfFrame, DtmfSender};
use std::error::Error;
use std::fmt;

pub const RTP_HEADER_LENGTH: usize = 12;
pub const PCMU_PAYLOAD_TYPE: u8 = 0;
pub const PCMU_CLOCK_RATE: u32 = 8_000;
pub const PCMU_SAMPLES_PER_PACKET: usize = 160;

const BIAS: i32 = 0x84;
const CLIP: i32 = 8_159;
const SEGMENT_ENDS: [i32; 8] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtpError {
    PacketTooShort { actual: usize, minimum: usize },
    InvalidVersion(u8),
    InvalidHeader(String),
    UnsupportedPayloadType(u8),
    InvalidPayloadLength { expected: usize, actual: usize },
    InvalidSampleRate(u32),
}

impl fmt::Display for RtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort { actual, minimum } => {
                write!(
                    formatter,
                    "RTP packet is too short: {actual} bytes, \
                     expected at least {minimum}"
                )
            }
            Self::InvalidVersion(version) => {
                write!(formatter, "Unsupported RTP version: {version}")
            }
            Self::InvalidHeader(reason) => {
                write!(formatter, "Invalid RTP header: {reason}")
            }
            Self::UnsupportedPayloadType(payload_type) => {
                write!(formatter, "Unsupported RTP payload type: {payload_type}")
            }
            Self::InvalidPayloadLength { expected, actual } => {
                write!(
                    formatter,
                    "Invalid RTP payload length: {actual}, \
                     expected {expected}"
                )
            }
            Self::InvalidSampleRate(sample_rate) => {
                write!(formatter, "Invalid audio sample rate: {sample_rate}")
            }
        }
    }
}

impl Error for RtpError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    pub fn parse(data: &[u8]) -> Result<Self, RtpError> {
        if data.len() < RTP_HEADER_LENGTH {
            return Err(RtpError::PacketTooShort {
                actual: data.len(),
                minimum: RTP_HEADER_LENGTH,
            });
        }

        let version = data[0] >> 6;

        if version != 2 {
            return Err(RtpError::InvalidVersion(version));
        }

        let has_padding = (data[0] & 0x20) != 0;
        let has_extension = (data[0] & 0x10) != 0;
        let csrc_count = usize::from(data[0] & 0x0F);

        let csrc_length = csrc_count
            .checked_mul(4)
            .ok_or_else(|| RtpError::InvalidHeader("CSRC list length overflow".to_owned()))?;

        let mut offset = RTP_HEADER_LENGTH
            .checked_add(csrc_length)
            .ok_or_else(|| RtpError::InvalidHeader("RTP header offset overflow".to_owned()))?;

        if offset > data.len() {
            return Err(RtpError::InvalidHeader(
                "CSRC list exceeds packet length".to_owned(),
            ));
        }

        if has_extension {
            if data.len() - offset < 4 {
                return Err(RtpError::InvalidHeader(
                    "Truncated RTP extension header".to_owned(),
                ));
            }

            let extension_words =
                usize::from(u16::from_be_bytes([data[offset + 2], data[offset + 3]]));

            let extension_length = extension_words
                .checked_mul(4)
                .and_then(|length| length.checked_add(4))
                .ok_or_else(|| {
                    RtpError::InvalidHeader("RTP extension length overflow".to_owned())
                })?;

            offset = offset
                .checked_add(extension_length)
                .ok_or_else(|| RtpError::InvalidHeader("RTP offset overflow".to_owned()))?;

            if offset > data.len() {
                return Err(RtpError::InvalidHeader(
                    "Truncated RTP extension data".to_owned(),
                ));
            }
        }

        let mut payload_end = data.len();

        if has_padding {
            let padding_bytes = usize::from(data[data.len() - 1]);

            if padding_bytes == 0 || padding_bytes > data.len() - offset {
                return Err(RtpError::InvalidHeader(
                    "Invalid RTP padding length".to_owned(),
                ));
            }

            payload_end -= padding_bytes;
        }

        let marker = (data[1] & 0x80) != 0;
        let payload_type = data[1] & 0x7F;

        let sequence_number = u16::from_be_bytes([data[2], data[3]]);

        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);

        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        Ok(Self {
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            payload: data[offset..payload_end].to_vec(),
        })
    }

    pub fn serialize(&self) -> Result<Vec<u8>, RtpError> {
        if self.payload_type > 0x7F {
            return Err(RtpError::UnsupportedPayloadType(self.payload_type));
        }

        let mut bytes = Vec::with_capacity(RTP_HEADER_LENGTH + self.payload.len());

        bytes.push(0x80);

        let second_byte = if self.marker {
            0x80 | self.payload_type
        } else {
            self.payload_type
        };

        bytes.push(second_byte);

        bytes.extend_from_slice(&self.sequence_number.to_be_bytes());

        bytes.extend_from_slice(&self.timestamp.to_be_bytes());

        bytes.extend_from_slice(&self.ssrc.to_be_bytes());

        bytes.extend_from_slice(&self.payload);

        Ok(bytes)
    }
}

#[derive(Debug)]
pub struct PcmuStream {
    resampler: Resampler,
    pending_samples: Vec<f32>,
    next_sequence_number: u16,
    next_timestamp: u32,
    ssrc: u32,
    first_packet: bool,
    dtmf: DtmfSender,
}

impl PcmuStream {
    pub fn new(ssrc: u32, source_sample_rate: u32) -> Result<Self, RtpError> {
        Ok(Self {
            resampler: Resampler::new(source_sample_rate, PCMU_CLOCK_RATE)
                .ok_or(RtpError::InvalidSampleRate(source_sample_rate))?,
            pending_samples: Vec::new(),
            next_sequence_number: 0,
            next_timestamp: 0,
            ssrc,
            first_packet: true,
            dtmf: DtmfSender::default(),
        })
    }

    /// Sets the negotiated telephone-event payload type. Without one, digits
    /// are sent as tones inside the voice.
    pub fn set_dtmf_payload_type(&mut self, payload_type: Option<u8>) {
        self.dtmf = DtmfSender::new(payload_type);
    }

    /// Queues a keypad digit; false when it is not a keypad character or too
    /// many are waiting.
    pub fn queue_dtmf(&mut self, digit: char) -> bool {
        self.dtmf.queue(digit)
    }

    pub fn push_pcm(&mut self, samples: &[f32]) -> Vec<RtpPacket> {
        let resampled = self.resampler.push(samples);

        self.pending_samples.extend(resampled);

        let mut packets = Vec::new();

        while self.pending_samples.len() >= PCMU_SAMPLES_PER_PACKET {
            let chunk = self
                .pending_samples
                .drain(..PCMU_SAMPLES_PER_PACKET)
                .collect::<Vec<f32>>();

            // A digit being sent takes the place of the microphone; the
            // sequence numbers and the voice clock run on regardless.
            let voice = |payload: Vec<u8>| (PCMU_PAYLOAD_TYPE, payload, self.first_packet, None);
            let (payload_type, payload, marker, timestamp) =
                match self.dtmf.next_frame(self.next_timestamp) {
                    DtmfFrame::Voice => voice(encode_pcmu(&chunk)),
                    DtmfFrame::Tone(tone) => voice(encode_pcmu(&tone)),
                    DtmfFrame::Silence => voice(encode_pcmu(&[0.0; PCMU_SAMPLES_PER_PACKET])),
                    DtmfFrame::Event {
                        payload_type,
                        payload,
                        marker,
                        timestamp,
                    } => (payload_type, payload.to_vec(), marker, Some(timestamp)),
                };

            let packet = RtpPacket {
                marker,
                payload_type,
                sequence_number: self.next_sequence_number,
                timestamp: timestamp.unwrap_or(self.next_timestamp),
                ssrc: self.ssrc,
                payload,
            };

            self.first_packet = false;

            self.next_sequence_number = self.next_sequence_number.wrapping_add(1);

            self.next_timestamp = self
                .next_timestamp
                .wrapping_add(PCMU_SAMPLES_PER_PACKET as u32);

            packets.push(packet);
        }

        packets
    }
}

pub fn encode_pcmu(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .map(|sample| {
            let pcm = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i32;

            linear_to_ulaw(pcm)
        })
        .collect()
}

pub fn decode_pcmu(payload: &[u8]) -> Vec<f32> {
    payload
        .iter()
        .map(|sample_byte| {
            let value = !*sample_byte;

            let mantissa = i32::from(value & 0x0F) << 3;

            let segment = i32::from((value & 0x70) >> 4);

            let magnitude = (mantissa + BIAS) << segment;

            let pcm = if (value & 0x80) != 0 {
                BIAS - magnitude
            } else {
                magnitude - BIAS
            };

            let pcm = pcm.clamp(i32::from(i16::MIN), i32::from(i16::MAX));

            pcm as f32 / 32_768.0
        })
        .collect()
}

fn linear_to_ulaw(pcm: i32) -> u8 {
    let mut value = pcm >> 2;

    let mask = if value < 0 {
        value = -value;
        0x7F
    } else {
        0xFF
    };

    if value > CLIP {
        value = CLIP;
    }

    value += BIAS >> 2;

    let segment = SEGMENT_ENDS
        .iter()
        .position(|segment_end| value <= *segment_end)
        .unwrap_or(8) as i32;

    if segment >= 8 {
        return (0x7F ^ mask) as u8;
    }

    let encoded = (segment << 4) | ((value >> (segment + 1)) & 0x0F);

    (encoded as u8) ^ (mask as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_and_parses_rtp_packet() {
        let original = RtpPacket {
            marker: true,
            payload_type: PCMU_PAYLOAD_TYPE,
            sequence_number: 12_345,
            timestamp: 0x0102_0304,
            ssrc: 0x1122_3344,
            payload: vec![1, 2, 3, 4],
        };

        let bytes = original.serialize().expect("packet should serialize");

        let parsed = RtpPacket::parse(&bytes).expect("packet should parse");

        assert_eq!(parsed, original);
    }

    #[test]
    fn encodes_pcmu_silence() {
        let encoded = encode_pcmu(&[0.0]);

        assert_eq!(encoded, vec![0xFF]);

        let decoded = decode_pcmu(&encoded);

        assert_eq!(decoded[0], 0.0);
    }

    #[test]
    fn round_trips_pcmu_samples() {
        let original = [0.0, 0.25, -0.25, 0.5, -0.5, 0.75, -0.75];

        let encoded = encode_pcmu(&original);
        let decoded = decode_pcmu(&encoded);

        for (source, result) in original.iter().zip(decoded.iter()) {
            assert!(
                (source - result).abs() < 0.04,
                "source={source}, decoded={result}"
            );
        }
    }

    #[test]
    fn resamples_48k_to_8k() {
        // The plain linear converter this test used to cover picked input
        // samples 0 and 6. It was replaced by the filtering `Resampler`, which
        // delays the signal, so only the number of samples is the same.
        let input = (0_u8..7).map(f32::from).collect::<Vec<f32>>();

        let mut resampler = Resampler::new(48_000, 8_000).expect("resampler should be valid");

        let output = resampler.push(&input);

        assert_eq!(output.len(), 2);
    }

    #[test]
    fn digit_replaces_voice_packets_and_keeps_the_stream_continuous() {
        let mut stream = PcmuStream::new(1, 8_000).expect("PCMU stream should be valid");
        stream.set_dtmf_payload_type(Some(101));
        let voice = vec![0.25; 160];

        let before = stream.push_pcm(&voice);
        assert_eq!(before[0].payload_type, PCMU_PAYLOAD_TYPE);
        assert!(stream.queue_dtmf('7'));
        assert!(!stream.queue_dtmf('?'));

        let mut packets = before;
        for _ in 0..12 {
            packets.extend(stream.push_pcm(&voice));
        }
        // One voice packet, ten event packets, then voice again.
        let types: Vec<u8> = packets.iter().map(|packet| packet.payload_type).collect();
        assert_eq!(
            types,
            [0, 101, 101, 101, 101, 101, 101, 101, 101, 101, 101, 0, 0]
        );

        // Sequence numbers never skip.
        for (index, packet) in packets.iter().enumerate() {
            assert_eq!(usize::from(packet.sequence_number), index);
        }
        // Event packets all carry the time the digit started ...
        assert!(packets[1..11].iter().all(|packet| packet.timestamp == 160));
        assert!(packets[1].marker);
        assert!(packets[2..].iter().all(|packet| !packet.marker));
        assert_eq!(packets[1].payload[0], 7);
        // ... and the voice clock went on underneath.
        assert_eq!(packets[11].timestamp, 11 * 160);
    }

    #[test]
    fn digit_without_negotiation_is_carried_inside_voice_packets() {
        let mut stream = PcmuStream::new(1, 8_000).expect("PCMU stream should be valid");
        assert!(stream.queue_dtmf('1'));
        let silence = vec![0.0; 160];
        let packets: Vec<RtpPacket> = (0..12).flat_map(|_| stream.push_pcm(&silence)).collect();

        assert!(
            packets
                .iter()
                .all(|packet| packet.payload_type == PCMU_PAYLOAD_TYPE)
        );
        // The microphone is silent, so anything audible is the tone.
        let loud = |packet: &RtpPacket| {
            decode_pcmu(&packet.payload)
                .iter()
                .any(|sample| sample.abs() > 0.1)
        };
        let audible: Vec<bool> = packets.iter().map(loud).collect();
        assert_eq!(
            audible,
            [
                true, true, true, true, true, true, true, true, false, false, false, false
            ]
        );
    }

    #[test]
    fn zero_source_rate_is_refused() {
        assert_eq!(
            PcmuStream::new(1, 0).err(),
            Some(RtpError::InvalidSampleRate(0))
        );
    }

    #[test]
    fn packetizes_twenty_milliseconds_of_pcmu() {
        let mut stream = PcmuStream::new(0x1234_5678, 48_000).expect("PCMU stream should be valid");

        let samples = vec![0.0; 960];

        let first = stream.push_pcm(&samples);

        assert_eq!(first.len(), 1);
        assert_eq!(first[0].payload.len(), PCMU_SAMPLES_PER_PACKET);
        assert!(first[0].marker);
        assert_eq!(first[0].sequence_number, 0);
        assert_eq!(first[0].timestamp, 0);

        let second = stream.push_pcm(&samples);

        assert_eq!(second.len(), 1);
        assert!(!second[0].marker);
        assert_eq!(second[0].sequence_number, 1);
        assert_eq!(second[0].timestamp, PCMU_SAMPLES_PER_PACKET as u32);
    }
}
