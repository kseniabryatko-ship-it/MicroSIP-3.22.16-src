//! SRTP: the voice packets of a call encrypted and signed (RFC 3711), with
//! the keys exchanged in SDP `a=crypto` lines (RFC 4568, "SDES").
//!
//! Each side makes a master key and tells it to the other in its SDP; what a
//! side sends is protected with its own key. From a master key the session
//! keys are derived; the payload of every packet is encrypted with AES in
//! counter mode and the whole packet signed with HMAC-SHA1, the signature
//! cut to 80 or 32 bits and put at the end. The RTP header stays readable.
//!
//! The keys travel inside the SIP messages, so the conversation is private
//! only as far as the signalling is: use TLS for SIP with it.

use aes::Aes128;
use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::fmt;

const KEY_LENGTH: usize = 16;
const SALT_LENGTH: usize = 14;
const AUTH_KEY_LENGTH: usize = 20;
/// How far behind the newest packet a late one is still taken.
const REPLAY_WINDOW: u64 = 64;

/// Whether the audio of calls is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaEncryption {
    /// Never; a call that insists on encryption is refused.
    #[default]
    Off,
    /// When the other side can: offered next to plain audio, accepted
    /// when asked for.
    Optional,
    /// Always; a call that cannot be encrypted does not take place.
    Required,
}

impl MediaEncryption {
    pub const ALL: [MediaEncryption; 3] = [
        MediaEncryption::Off,
        MediaEncryption::Optional,
        MediaEncryption::Required,
    ];

    /// The name kept in the settings and typed in commands.
    pub const fn name(self) -> &'static str {
        match self {
            MediaEncryption::Off => "off",
            MediaEncryption::Optional => "optional",
            MediaEncryption::Required => "required",
        }
    }

    /// Reads a name in any letter case; anything unknown is `Off`.
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name().eq_ignore_ascii_case(name.trim()))
            .unwrap_or_default()
    }
}

/// How the packets are protected. Both use AES-128 in counter mode; they
/// differ in the length of the signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    AesCm128HmacSha1_80,
    AesCm128HmacSha1_32,
}

impl Suite {
    /// The name in an SDP `a=crypto` line.
    pub const fn name(self) -> &'static str {
        match self {
            Suite::AesCm128HmacSha1_80 => "AES_CM_128_HMAC_SHA1_80",
            Suite::AesCm128HmacSha1_32 => "AES_CM_128_HMAC_SHA1_32",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Suite::AesCm128HmacSha1_80, Suite::AesCm128HmacSha1_32]
            .into_iter()
            .find(|suite| suite.name().eq_ignore_ascii_case(name))
    }

    /// Bytes of signature at the end of each packet.
    pub const fn tag_length(self) -> usize {
        match self {
            Suite::AesCm128HmacSha1_80 => 10,
            Suite::AesCm128HmacSha1_32 => 4,
        }
    }
}

/// The secret one side of a call makes up and tells the other in SDP.
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey {
    key: [u8; KEY_LENGTH],
    salt: [u8; SALT_LENGTH],
}

impl MasterKey {
    /// A new key from the system's source of randomness.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0_u8; KEY_LENGTH + SALT_LENGTH];
        getrandom::fill(&mut bytes).map_err(|error| format!("no random numbers: {error}"))?;
        Ok(Self::from_bytes(&bytes))
    }

    fn from_bytes(bytes: &[u8; KEY_LENGTH + SALT_LENGTH]) -> Self {
        let mut key = [0_u8; KEY_LENGTH];
        let mut salt = [0_u8; SALT_LENGTH];
        key.copy_from_slice(&bytes[..KEY_LENGTH]);
        salt.copy_from_slice(&bytes[KEY_LENGTH..]);
        Self { key, salt }
    }

    /// Reads the base64 text after `inline:` in a crypto line.
    pub fn from_inline(text: &str) -> Option<Self> {
        let bytes = base64_decode(text)?;
        let bytes: [u8; KEY_LENGTH + SALT_LENGTH] = bytes.try_into().ok()?;
        Some(Self::from_bytes(&bytes))
    }

    /// The key as written after `inline:`.
    pub fn to_inline(&self) -> String {
        let mut bytes = self.key.to_vec();
        bytes.extend_from_slice(&self.salt);
        base64_encode(&bytes)
    }
}

/// Keys are never printed, not even into a debug log.
impl fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MasterKey(hidden)")
    }
}

/// One `a=crypto` line: `1 AES_CM_128_HMAC_SHA1_80 inline:...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoLine {
    pub tag: u32,
    pub suite: Suite,
    pub key: MasterKey,
}

impl CryptoLine {
    /// Reads the value of an `a=crypto` attribute. A line with a suite this
    /// phone does not have, with several keys or with a key identifier
    /// (MKI) is not usable and gives `None`.
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split_whitespace();
        let tag = parts.next()?.parse().ok()?;
        let suite = Suite::from_name(parts.next()?)?;
        let keys = parts.next()?;
        if keys.contains(';') {
            return None;
        }
        let inline = keys.strip_prefix("inline:")?;
        // After the key may come a lifetime (`|2^31`) and an MKI (`|1:4`).
        let mut fields = inline.split('|');
        let key = MasterKey::from_inline(fields.next()?)?;
        if fields.any(|field| field.contains(':')) {
            return None;
        }
        Some(Self { tag, suite, key })
    }

    /// The value for an `a=crypto` attribute.
    pub fn format(&self) -> String {
        format!(
            "{} {} inline:{}",
            self.tag,
            self.suite.name(),
            self.key.to_inline()
        )
    }
}

/// Everything a call's audio needs to be encrypted both ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtpKeys {
    pub suite: Suite,
    /// Protects what this phone sends.
    pub local: MasterKey,
    /// Protects what the other side sends.
    pub remote: MasterKey,
}

/// The keys of one direction, derived from its master key.
struct SessionKeys {
    cipher: Aes128,
    salt: [u8; SALT_LENGTH],
    auth: [u8; AUTH_KEY_LENGTH],
    tag_length: usize,
}

impl SessionKeys {
    fn derive(master: &MasterKey, suite: Suite) -> Self {
        let master_cipher = Aes128::new(&Array::from(master.key));
        let derive = |label: u8, output: &mut [u8]| {
            // The label is put above the 48-bit packet index (which is
            // zero here: keys are derived once) and mixed with the salt.
            let mut block = [0_u8; 16];
            block[..SALT_LENGTH].copy_from_slice(&master.salt);
            block[7] ^= label;
            keystream(&master_cipher, &block, output);
        };
        let mut key = [0_u8; KEY_LENGTH];
        let mut auth = [0_u8; AUTH_KEY_LENGTH];
        let mut salt = [0_u8; SALT_LENGTH];
        derive(0, &mut key);
        derive(1, &mut auth);
        derive(2, &mut salt);
        Self {
            cipher: Aes128::new(&Array::from(key)),
            salt,
            auth,
            tag_length: suite.tag_length(),
        }
    }

    /// Encrypts or decrypts (the same operation) the payload of a packet.
    fn crypt(&self, ssrc: u32, index: u64, payload: &mut [u8]) {
        let mut block = [0_u8; 16];
        block[..SALT_LENGTH].copy_from_slice(&self.salt);
        for (slot, byte) in block[4..8].iter_mut().zip(ssrc.to_be_bytes()) {
            *slot ^= byte;
        }
        for (slot, byte) in block[8..14].iter_mut().zip(&index.to_be_bytes()[2..]) {
            *slot ^= byte;
        }
        let mut stream = vec![0_u8; payload.len()];
        keystream(&self.cipher, &block, &mut stream);
        for (byte, key) in payload.iter_mut().zip(stream) {
            *byte ^= key;
        }
    }

    /// The signature of a packet (header and encrypted payload) with the
    /// rollover counter, cut to the suite's length.
    fn tag(&self, packet: &[u8], rollover: u32) -> Vec<u8> {
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&self.auth) else {
            return Vec::new();
        };
        mac.update(packet);
        mac.update(&rollover.to_be_bytes());
        mac.finalize().into_bytes()[..self.tag_length].to_vec()
    }
}

/// AES in counter mode: `output` is filled with the encryptions of
/// `start`, `start + 1`, ... (the counter is the last two bytes).
fn keystream(cipher: &Aes128, start: &[u8; 16], output: &mut [u8]) {
    for (counter, chunk) in output.chunks_mut(16).enumerate() {
        let mut block = *start;
        let low = u16::from_be_bytes([block[14], block[15]]).wrapping_add(counter as u16);
        block[14..].copy_from_slice(&low.to_be_bytes());
        let mut block = Array::from(block);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block[..chunk.len()]);
    }
}

/// Length of the RTP header of `packet` (with its CSRC list and extension),
/// its sequence number and its SSRC.
fn header(packet: &[u8]) -> Option<(usize, u16, u32)> {
    if packet.len() < 12 || packet[0] >> 6 != 2 {
        return None;
    }
    let mut length = 12 + 4 * usize::from(packet[0] & 0x0F);
    if packet[0] & 0x10 != 0 {
        let words = packet.get(length + 2..length + 4)?;
        length += 4 + 4 * usize::from(u16::from_be_bytes([words[0], words[1]]));
    }
    if packet.len() < length {
        return None;
    }
    let sequence = u16::from_be_bytes([packet[2], packet[3]]);
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
    Some((length, sequence, ssrc))
}

/// Protects the packets this phone sends.
pub struct SrtpSender {
    keys: SessionKeys,
    rollover: u32,
    last_sequence: Option<u16>,
}

impl SrtpSender {
    pub fn new(master: &MasterKey, suite: Suite) -> Self {
        Self {
            keys: SessionKeys::derive(master, suite),
            rollover: 0,
            last_sequence: None,
        }
    }

    /// Turns an RTP packet into an SRTP one. Packets must be given in the
    /// order of their sequence numbers. `None` for what is not RTP.
    pub fn protect(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        let (header_length, sequence, ssrc) = header(packet)?;
        // The 16-bit sequence number started again from zero.
        if self.last_sequence.is_some_and(|last| sequence < last) {
            self.rollover = self.rollover.wrapping_add(1);
        }
        self.last_sequence = Some(sequence);
        let index = (u64::from(self.rollover) << 16) | u64::from(sequence);
        let mut protected = packet.to_vec();
        self.keys
            .crypt(ssrc, index, &mut protected[header_length..]);
        let tag = self.keys.tag(&protected, self.rollover);
        protected.extend_from_slice(&tag);
        Some(protected)
    }
}

/// Checks and decrypts the packets of the other side.
pub struct SrtpReceiver {
    keys: SessionKeys,
    /// The sender being followed; another one starts afresh.
    ssrc: Option<u32>,
    rollover: u32,
    highest_sequence: u16,
    /// Bit `n` is set when the packet `n` behind the newest has been seen.
    seen: u64,
}

impl SrtpReceiver {
    pub fn new(master: &MasterKey, suite: Suite) -> Self {
        Self {
            keys: SessionKeys::derive(master, suite),
            ssrc: None,
            rollover: 0,
            highest_sequence: 0,
            seen: 0,
        }
    }

    /// Turns an SRTP packet back into RTP. `None` when the signature is
    /// wrong (not from the other side, or damaged), when the packet was
    /// already received, or when it is not SRTP at all.
    pub fn unprotect(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        let (header_length, sequence, ssrc) = header(packet)?;
        let tag_length = self.keys.tag_length;
        let body_end = packet.len().checked_sub(tag_length)?;
        if body_end < header_length {
            return None;
        }
        let known = self.ssrc == Some(ssrc);
        // Which round of sequence numbers the packet belongs to.
        let (rollover, newest) = if !known {
            (0, true)
        } else if self.highest_sequence < 0x8000 {
            if sequence > self.highest_sequence && sequence - self.highest_sequence > 0x8000 {
                (self.rollover.wrapping_sub(1), false)
            } else {
                (self.rollover, sequence > self.highest_sequence)
            }
        } else if sequence < self.highest_sequence - 0x8000 {
            (self.rollover.wrapping_add(1), true)
        } else {
            (self.rollover, sequence > self.highest_sequence)
        };
        let index = (u64::from(rollover) << 16) | u64::from(sequence);
        let highest = (u64::from(self.rollover) << 16) | u64::from(self.highest_sequence);
        if known && !newest {
            let behind = highest.checked_sub(index)?;
            if behind >= REPLAY_WINDOW || self.seen & (1 << behind) != 0 {
                return None;
            }
        }

        let (signed, tag) = packet.split_at(body_end);
        let expected = self.keys.tag(signed, rollover);
        // Compared without stopping at the first difference.
        let difference = expected
            .iter()
            .zip(tag)
            .fold(0_u8, |sum, (left, right)| sum | (left ^ right));
        if expected.len() != tag.len() || difference != 0 {
            return None;
        }

        if !known {
            self.ssrc = Some(ssrc);
            self.rollover = 0;
            self.highest_sequence = sequence;
            self.seen = 1;
        } else if newest {
            let ahead = index - highest;
            self.seen = if ahead >= REPLAY_WINDOW {
                1
            } else {
                (self.seen << ahead) | 1
            };
            self.rollover = rollover;
            self.highest_sequence = sequence;
        } else {
            self.seen |= 1 << (highest - index);
        }

        let mut plain = signed.to_vec();
        self.keys.crypt(ssrc, index, &mut plain[header_length..]);
        Some(plain)
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let group = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let sextets = [
            group[0] >> 2,
            ((group[0] & 0x03) << 4) | (group[1] >> 4),
            ((group[1] & 0x0F) << 2) | (group[2] >> 6),
            group[2] & 0x3F,
        ];
        for (position, sextet) in sextets.iter().enumerate() {
            if position <= chunk.len() {
                text.push(char::from(BASE64[usize::from(*sextet)]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut bytes = Vec::new();
    let (mut bits, mut count) = (0_u32, 0);
    for character in text.bytes() {
        let value = BASE64.iter().position(|known| *known == character)?;
        bits = (bits << 6) | value as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        digits
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    /// The master key of RFC 3711, appendix B.3.
    fn rfc_master() -> MasterKey {
        let mut bytes = hex("E1F97A0D3E018BE0D64FA32C06DE4139");
        bytes.extend(hex("0EC675AD498AFEEBB6960B3AABE6"));
        MasterKey::from_bytes(&bytes.try_into().unwrap())
    }

    #[test]
    fn counter_mode_gives_the_keystream_of_the_standard() {
        // RFC 3711, appendix B.2.
        let key: [u8; 16] = hex("2B7E151628AED2A6ABF7158809CF4F3C").try_into().unwrap();
        let start: [u8; 16] = hex("F0F1F2F3F4F5F6F7F8F9FAFBFCFD0000").try_into().unwrap();
        let cipher = Aes128::new(&Array::from(key));
        let mut stream = [0_u8; 48];
        keystream(&cipher, &start, &mut stream);
        assert_eq!(
            stream.to_vec(),
            hex("E03EAD0935C95E80E166B16DD92B4EB4
                 D23513162B02D0F72A43A2FE4A5F97AB
                 41E95B3BB0A2E8DD477901E4FCA894C0")
        );
    }

    #[test]
    fn session_keys_are_derived_as_in_the_standard() {
        // RFC 3711, appendix B.3. The cipher key cannot be read back, so
        // it is checked by what it encrypts.
        let keys = SessionKeys::derive(&rfc_master(), Suite::AesCm128HmacSha1_80);
        assert_eq!(keys.salt.to_vec(), hex("30CBBC08863D8C85D49DB34A9AE1"));
        assert_eq!(
            keys.auth.to_vec(),
            hex("CEBE321F6FF7716B6FD4AB49AF256A156D38BAA4")
        );
        let expected: [u8; 16] = hex("C61E7A93744F39EE10734AFE3FF7A087").try_into().unwrap();
        let expected = Aes128::new(&Array::from(expected));
        let (mut ours, mut theirs) = ([0_u8; 16], [0_u8; 16]);
        keystream(&keys.cipher, &[7; 16], &mut ours);
        keystream(&expected, &[7; 16], &mut theirs);
        assert_eq!(ours, theirs);
    }

    fn rtp(sequence: u16, ssrc: u32, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x80, 0];
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&(u32::from(sequence) * 160).to_be_bytes());
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn pair(suite: Suite) -> (SrtpSender, SrtpReceiver) {
        let key = MasterKey::generate().unwrap();
        (SrtpSender::new(&key, suite), SrtpReceiver::new(&key, suite))
    }

    #[test]
    fn protected_packets_come_back_and_hide_the_voice() {
        for suite in [Suite::AesCm128HmacSha1_80, Suite::AesCm128HmacSha1_32] {
            let (mut sender, mut receiver) = pair(suite);
            for sequence in 0..300_u16 {
                let plain = rtp(sequence, 0x1234_5678, &[0x55; 160]);
                let protected = sender.protect(&plain).unwrap();
                assert_eq!(protected.len(), plain.len() + suite.tag_length());
                // The header is readable, the payload is not the voice.
                assert_eq!(protected[..12], plain[..12]);
                assert_ne!(protected[12..172], plain[12..]);
                assert_eq!(receiver.unprotect(&protected), Some(plain));
            }
        }
    }

    #[test]
    fn a_changed_or_foreign_packet_is_refused() {
        let (mut sender, mut receiver) = pair(Suite::AesCm128HmacSha1_80);
        let good = sender.protect(&rtp(1, 9, b"voice")).unwrap();
        for position in [1, 3, 13, good.len() - 1] {
            let mut bad = good.clone();
            bad[position] ^= 0x01;
            assert_eq!(receiver.unprotect(&bad), None, "byte {position}");
        }
        // Protected with somebody else's key.
        let (mut stranger, _) = pair(Suite::AesCm128HmacSha1_80);
        let foreign = stranger.protect(&rtp(1, 9, b"voice")).unwrap();
        assert_eq!(receiver.unprotect(&foreign), None);
        // Plain RTP, too short, not RTP.
        assert_eq!(receiver.unprotect(&rtp(1, 9, b"voice")), None);
        assert_eq!(receiver.unprotect(&[0x80, 0, 0, 1]), None);
        assert_eq!(receiver.unprotect(&[0; 40]), None);
        // The untouched packet is still good after all that.
        assert_eq!(receiver.unprotect(&good), Some(rtp(1, 9, b"voice")));
    }

    #[test]
    fn a_packet_is_taken_once_and_late_ones_within_the_window() {
        let (mut sender, mut receiver) = pair(Suite::AesCm128HmacSha1_80);
        let packets: Vec<Vec<u8>> = (0..100_u16)
            .map(|sequence| sender.protect(&rtp(sequence, 5, &[1, 2, 3])).unwrap())
            .collect();
        assert!(receiver.unprotect(&packets[0]).is_some());
        assert!(receiver.unprotect(&packets[2]).is_some());
        // Recorded and sent again by someone in between.
        assert_eq!(receiver.unprotect(&packets[2]), None);
        // Late, but not seen before.
        assert!(receiver.unprotect(&packets[1]).is_some());
        assert_eq!(receiver.unprotect(&packets[1]), None);
        assert!(receiver.unprotect(&packets[99]).is_some());
        // Too far behind the newest to be told from a replay.
        assert_eq!(receiver.unprotect(&packets[10]), None);
        assert!(receiver.unprotect(&packets[60]).is_some());
    }

    #[test]
    fn long_calls_pass_the_end_of_the_sequence_numbers() {
        let (mut sender, mut receiver) = pair(Suite::AesCm128HmacSha1_80);
        // Start close to the end, as a sender with a random start does.
        let mut late = None;
        for step in 0..70_000_u32 {
            let sequence = (65_000 + step) as u16;
            let plain = rtp(sequence, 77, &[step as u8; 20]);
            let protected = sender.protect(&plain).unwrap();
            // One packet from just before the turn arrives after it.
            if sequence == 65_535 {
                late = Some((protected, plain));
                continue;
            }
            assert_eq!(receiver.unprotect(&protected), Some(plain), "step {step}");
            if sequence == 3
                && let Some((protected, plain)) = late.take()
            {
                assert_eq!(receiver.unprotect(&protected), Some(plain));
            }
        }
        assert_eq!(sender.rollover, 2);
        assert_eq!(receiver.rollover, 2);
    }

    #[test]
    fn a_new_sender_with_the_same_key_starts_afresh() {
        // After a transfer the exchange may send from another SSRC.
        let key = MasterKey::generate().unwrap();
        let suite = Suite::AesCm128HmacSha1_80;
        let mut receiver = SrtpReceiver::new(&key, suite);
        let mut first = SrtpSender::new(&key, suite);
        let mut second = SrtpSender::new(&key, suite);
        for sequence in 500..520 {
            let packet = first.protect(&rtp(sequence, 1, b"one")).unwrap();
            assert!(receiver.unprotect(&packet).is_some());
        }
        for sequence in 0..20 {
            let packet = second.protect(&rtp(sequence, 2, b"two")).unwrap();
            assert_eq!(receiver.unprotect(&packet), Some(rtp(sequence, 2, b"two")));
        }
    }

    #[test]
    fn header_extensions_and_contributors_stay_in_the_clear() {
        let (mut sender, mut receiver) = pair(Suite::AesCm128HmacSha1_80);
        // One CSRC and a one-word extension before the payload.
        let mut plain = vec![0x91, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 9];
        plain.extend_from_slice(&[1, 1, 1, 1]);
        plain.extend_from_slice(&[0xBE, 0xDE, 0, 1, 2, 2, 2, 2]);
        plain.extend_from_slice(b"payload");
        let protected = sender.protect(&plain).unwrap();
        assert_eq!(protected[..24], plain[..24]);
        assert_ne!(protected[24..31], plain[24..]);
        assert_eq!(receiver.unprotect(&protected), Some(plain));
        // A header that says it is longer than the packet.
        assert!(
            sender
                .protect(&[0x90, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1])
                .is_none()
        );
    }

    #[test]
    fn crypto_lines_are_written_and_read() {
        let key = rfc_master();
        let line = CryptoLine {
            tag: 1,
            suite: Suite::AesCm128HmacSha1_80,
            key: key.clone(),
        };
        let text = line.format();
        // The key in base64, as any other program writes it.
        assert_eq!(
            text,
            "1 AES_CM_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm"
        );
        assert_eq!(CryptoLine::parse(&text), Some(line));

        // A lifetime after the key is allowed; the 32-bit suite is known.
        let with_lifetime = format!("2 AES_CM_128_HMAC_SHA1_32 inline:{}|2^31", key.to_inline());
        let parsed = CryptoLine::parse(&with_lifetime).unwrap();
        assert_eq!((parsed.tag, parsed.suite), (2, Suite::AesCm128HmacSha1_32));
        assert_eq!(parsed.key, key);

        // What this phone cannot use.
        for bad in [
            "",
            "1 AES_CM_128_HMAC_SHA1_80",
            "x AES_CM_128_HMAC_SHA1_80 inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm",
            "1 AEAD_AES_256_GCM inline:4fl6DT4Bi+DWT6MsBt5BOQ7Gda1Jiv7rtpYLOqvm",
            "1 AES_CM_128_HMAC_SHA1_80 inline:c2hvcnQ=",
            "1 AES_CM_128_HMAC_SHA1_80 inline:not base64 at all!",
        ] {
            assert_eq!(CryptoLine::parse(bad), None, "{bad}");
        }
        let mki = format!(
            "1 AES_CM_128_HMAC_SHA1_80 inline:{}|2^20|1:4",
            key.to_inline()
        );
        assert_eq!(CryptoLine::parse(&mki), None);
    }

    #[test]
    fn encryption_modes_are_read_and_written() {
        for mode in MediaEncryption::ALL {
            assert_eq!(MediaEncryption::from_name(mode.name()), mode);
        }
        assert_eq!(
            MediaEncryption::from_name(" Required "),
            MediaEncryption::Required
        );
        assert_eq!(MediaEncryption::from_name("zrtp"), MediaEncryption::Off);
        assert_eq!(MediaEncryption::default(), MediaEncryption::Off);
    }

    #[test]
    fn base64_follows_the_standard_examples() {
        // RFC 4648, section 10.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(base64_decode(encoded), Some(plain.as_bytes().to_vec()));
        }
        assert_eq!(base64_decode("Zm9v!"), None);
    }

    #[test]
    fn keys_are_random_and_never_printed() {
        let (first, second) = (
            MasterKey::generate().unwrap(),
            MasterKey::generate().unwrap(),
        );
        assert_ne!(first, second);
        assert_eq!(first.to_inline().len(), 40);
        let keys = SrtpKeys {
            suite: Suite::AesCm128HmacSha1_80,
            local: first.clone(),
            remote: second,
        };
        let printed = format!("{keys:?}");
        assert!(!printed.contains(&first.to_inline()), "{printed}");
        assert!(printed.contains("hidden"));
    }
}
