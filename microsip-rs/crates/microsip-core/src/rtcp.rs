//! RTCP (RFC 3550): the reports both sides of a call send each other every
//! few seconds about the audio they receive. This phone reports how much of
//! the other side's audio it lost and how unevenly it arrived; from the
//! other side's reports it learns how much of *its own* voice was lost on
//! the way, and the delay of the connection there and back.
//!
//! [`Reception`] keeps the counts of the packets received from one sender
//! (RFC 3550, appendix A.1, A.3 and A.8), [`build_report`] makes a compound
//! report and [`parse`] reads one.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const VERSION: u8 = 2;
const SENDER_REPORT: u8 = 200;
const RECEIVER_REPORT: u8 = 201;
const SOURCE_DESCRIPTION: u8 = 202;
const BYE: u8 = 203;
/// SDES item: canonical name.
const CNAME: u8 = 1;
/// Seconds from 1900 (NTP) to 1970 (Unix).
const NTP_OFFSET: u64 = 2_208_988_800;
/// A jump in sequence numbers bigger than this is a new stream.
const MAX_DROPOUT: u16 = 3_000;
const MAX_MISORDER: u16 = 100;

/// True when a datagram is RTCP rather than RTP: the second byte is a
/// report type, which no RTP payload type of a call uses.
pub fn is_rtcp(datagram: &[u8]) -> bool {
    datagram.len() >= 8 && datagram[0] >> 6 == VERSION && (200..=204).contains(&datagram[1])
}

/// The current time as NTP: seconds since 1900 in the upper half, the
/// fraction of a second in the lower.
pub fn ntp_now() -> u64 {
    let since_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    ntp_from(since_unix)
}

fn ntp_from(since_unix: Duration) -> u64 {
    let seconds = since_unix.as_secs() + NTP_OFFSET;
    let fraction = (u64::from(since_unix.subsec_nanos()) << 32) / 1_000_000_000;
    (seconds << 32) | fraction
}

/// The middle 32 bits of an NTP time, as reports quote it.
pub fn ntp_middle(ntp: u64) -> u32 {
    (ntp >> 16) as u32
}

/// What the sender of a report says about the packets it has sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderInfo {
    pub ntp: u64,
    /// The RTP timestamp that goes with `ntp`.
    pub rtp_timestamp: u32,
    pub packets: u32,
    pub octets: u32,
}

/// One report block: what a receiver says about one sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportBlock {
    /// The sender reported on.
    pub ssrc: u32,
    /// Share of packets lost since the last report, in 1/256.
    pub fraction_lost: u8,
    /// Packets lost in all (may be negative with duplicates).
    pub cumulative_lost: i32,
    pub highest_sequence: u32,
    /// Interarrival jitter, in timestamp units of the sender's clock.
    pub jitter: u32,
    /// Middle of the NTP time of the sender's last report; 0 for none.
    pub last_report: u32,
    /// Time since that report, in 1/65536 s.
    pub delay_since_last: u32,
}

/// What a received compound report holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Who sent it.
    pub ssrc: u32,
    /// Present in a sender report.
    pub sender: Option<SenderInfo>,
    pub blocks: Vec<ReportBlock>,
    /// The sender said it is leaving.
    pub bye: bool,
}

/// What the other side reported about this phone's audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteQuality {
    /// Share of this phone's packets lost on the way, in percent, over the
    /// last reporting interval.
    pub lost_percent: f32,
    /// Packets lost in all.
    pub lost_total: i32,
    /// How unevenly this phone's packets arrived there, in milliseconds.
    pub jitter_ms: f32,
    /// Time there and back, when it could be measured.
    pub round_trip_ms: Option<f32>,
}

impl RemoteQuality {
    /// Reads the block about this phone. `clock_rate` is that of the codec
    /// this phone sends; `now` is the NTP time the report arrived.
    pub fn from_block(block: &ReportBlock, clock_rate: u32, now: u64) -> Self {
        let round_trip_ms = (block.last_report != 0).then(|| {
            let units = ntp_middle(now)
                .wrapping_sub(block.last_report)
                .wrapping_sub(block.delay_since_last);
            // A clock that jumped would give nonsense; it is left out.
            (units < 65_536 * 10).then_some(units as f32 * 1_000.0 / 65_536.0)
        });
        Self {
            lost_percent: f32::from(block.fraction_lost) * 100.0 / 256.0,
            lost_total: block.cumulative_lost,
            jitter_ms: block.jitter as f32 * 1_000.0 / clock_rate.max(1) as f32,
            round_trip_ms: round_trip_ms.flatten(),
        }
    }
}

/// Counts of the packets received from one sender.
#[derive(Debug, Clone, Default)]
pub struct Reception {
    ssrc: Option<u32>,
    base_sequence: u32,
    max_sequence: u16,
    cycles: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
    /// Difference between arrival and RTP time of the last packet.
    transit: Option<u32>,
    /// In timestamp units, times 16 (as RFC 3550 keeps it).
    jitter_16: u64,
    /// Packets that do not follow, while a jump is being checked.
    probation: Option<u16>,
}

impl Reception {
    /// Counts a received packet. `arrival` is the time it arrived, in units
    /// of its RTP clock.
    pub fn on_packet(&mut self, ssrc: u32, sequence: u16, timestamp: u32, arrival: u32) {
        if self.ssrc != Some(ssrc) {
            *self = Self {
                ssrc: Some(ssrc),
                ..Self::default()
            };
            self.restart(sequence);
        } else {
            let delta = sequence.wrapping_sub(self.max_sequence);
            if delta < MAX_DROPOUT {
                if sequence < self.max_sequence {
                    self.cycles += 1 << 16;
                }
                self.max_sequence = sequence;
                self.probation = None;
            } else if delta <= u16::MAX - MAX_MISORDER {
                // A big jump: the sender may have restarted. Two packets in
                // a row from the new place confirm it.
                if self.probation == Some(sequence) {
                    self.restart(sequence);
                    self.transit = None;
                } else {
                    self.probation = Some(sequence.wrapping_add(1));
                    return;
                }
            }
            // Otherwise a duplicate or a packet a little late: counted, the
            // highest number stays.
        }
        self.received = self.received.wrapping_add(1);

        let transit = arrival.wrapping_sub(timestamp);
        if let Some(last) = self.transit {
            let difference = u64::from((transit.wrapping_sub(last) as i32).unsigned_abs());
            // J += (|D| - J) / 16, kept times 16.
            self.jitter_16 =
                self.jitter_16 + difference - self.jitter_16.div_ceil(16).min(self.jitter_16);
        }
        self.transit = Some(transit);
    }

    fn restart(&mut self, sequence: u16) {
        self.base_sequence = u32::from(sequence);
        self.max_sequence = sequence;
        self.cycles = 0;
        self.received = 0;
        self.expected_prior = 0;
        self.received_prior = 0;
        self.probation = None;
    }

    /// The report block about this sender, and the start of a new interval
    /// for the next one. `None` before any packet.
    pub fn report(&mut self, last_report: u32, delay_since_last: u32) -> Option<ReportBlock> {
        let ssrc = self.ssrc?;
        let highest = self.cycles + u32::from(self.max_sequence);
        let expected = highest.wrapping_sub(self.base_sequence).wrapping_add(1);
        let lost = i64::from(expected) - i64::from(self.received);
        let expected_interval = expected.wrapping_sub(self.expected_prior);
        let received_interval = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = i64::from(expected_interval) - i64::from(received_interval);
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / i64::from(expected_interval)).min(255) as u8
        };
        Some(ReportBlock {
            ssrc,
            fraction_lost,
            // 24 bits, signed.
            cumulative_lost: lost.clamp(-0x80_0000, 0x7F_FFFF) as i32,
            highest_sequence: highest,
            jitter: (self.jitter_16 / 16) as u32,
            last_report,
            delay_since_last,
        })
    }
}

/// A compound report: a sender report when `sender` is given (this phone
/// has sent audio), otherwise a receiver report; then the canonical name.
pub fn build_report(
    ssrc: u32,
    sender: Option<SenderInfo>,
    block: Option<ReportBlock>,
    cname: &str,
) -> Vec<u8> {
    let count = u8::from(block.is_some());
    let mut packet = Vec::with_capacity(80);
    let kind = if sender.is_some() {
        SENDER_REPORT
    } else {
        RECEIVER_REPORT
    };
    packet.push((VERSION << 6) | count);
    packet.push(kind);
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(&ssrc.to_be_bytes());
    if let Some(info) = sender {
        packet.extend_from_slice(&info.ntp.to_be_bytes());
        packet.extend_from_slice(&info.rtp_timestamp.to_be_bytes());
        packet.extend_from_slice(&info.packets.to_be_bytes());
        packet.extend_from_slice(&info.octets.to_be_bytes());
    }
    if let Some(block) = block {
        packet.extend_from_slice(&block.ssrc.to_be_bytes());
        let lost = (block.cumulative_lost as u32) & 0x00FF_FFFF;
        packet.push(block.fraction_lost);
        packet.extend_from_slice(&lost.to_be_bytes()[1..]);
        packet.extend_from_slice(&block.highest_sequence.to_be_bytes());
        packet.extend_from_slice(&block.jitter.to_be_bytes());
        packet.extend_from_slice(&block.last_report.to_be_bytes());
        packet.extend_from_slice(&block.delay_since_last.to_be_bytes());
    }
    set_length(&mut packet, 0);

    // Source description with the canonical name, padded to whole words.
    let start = packet.len();
    let name = &cname.as_bytes()[..cname.len().min(255)];
    packet.push((VERSION << 6) | 1);
    packet.push(SOURCE_DESCRIPTION);
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.push(CNAME);
    packet.push(name.len() as u8);
    packet.extend_from_slice(name);
    // The item list ends with a zero, then zeros up to a word boundary.
    packet.push(0);
    while (packet.len() - start) % 4 != 0 {
        packet.push(0);
    }
    set_length(&mut packet, start);
    packet
}

/// A goodbye: the sender is leaving the call.
pub fn build_bye(ssrc: u32) -> Vec<u8> {
    let mut packet = vec![(VERSION << 6) | 1, BYE, 0, 0];
    packet.extend_from_slice(&ssrc.to_be_bytes());
    set_length(&mut packet, 0);
    packet
}

/// Writes the length field of the packet starting at `start`: its size in
/// 32-bit words, minus one.
fn set_length(packet: &mut [u8], start: usize) {
    let words = ((packet.len() - start) / 4 - 1) as u16;
    packet[start + 2..start + 4].copy_from_slice(&words.to_be_bytes());
}

/// Reads a compound report. `None` when it is not RTCP or is damaged.
pub fn parse(datagram: &[u8]) -> Option<Report> {
    if !is_rtcp(datagram) {
        return None;
    }
    let mut report = Report::default();
    let mut rest = datagram;
    while rest.len() >= 4 {
        if rest[0] >> 6 != VERSION {
            return None;
        }
        let count = usize::from(rest[0] & 0x1F);
        let kind = rest[1];
        let length = (usize::from(u16::from_be_bytes([rest[2], rest[3]])) + 1) * 4;
        let packet = rest.get(..length)?;
        rest = &rest[length..];
        let word = |at: usize| -> Option<u32> {
            packet
                .get(at..at + 4)
                .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        };
        match kind {
            SENDER_REPORT | RECEIVER_REPORT => {
                report.ssrc = word(4)?;
                let mut at = 8;
                if kind == SENDER_REPORT {
                    let ntp = (u64::from(word(8)?) << 32) | u64::from(word(12)?);
                    report.sender = Some(SenderInfo {
                        ntp,
                        rtp_timestamp: word(16)?,
                        packets: word(20)?,
                        octets: word(24)?,
                    });
                    at = 28;
                }
                for _ in 0..count {
                    let second = word(at + 4)?;
                    // The lost count is 24 bits, signed.
                    let lost = ((second << 8) as i32) >> 8;
                    report.blocks.push(ReportBlock {
                        ssrc: word(at)?,
                        fraction_lost: (second >> 24) as u8,
                        cumulative_lost: lost,
                        highest_sequence: word(at + 8)?,
                        jitter: word(at + 12)?,
                        last_report: word(at + 16)?,
                        delay_since_last: word(at + 20)?,
                    });
                    at += 24;
                }
            }
            BYE => report.bye = true,
            // Source descriptions and anything else are skipped.
            _ => {}
        }
    }
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtcp_is_told_from_rtp() {
        assert!(is_rtcp(&build_bye(1)));
        assert!(is_rtcp(&build_report(1, None, None, "x")));
        // An RTP packet of PCMU, and one with the marker bit on PCMA.
        assert!(!is_rtcp(&[0x80, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]));
        assert!(!is_rtcp(&[0x80, 0x88, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]));
        assert!(!is_rtcp(&[0x80, 200]));
    }

    #[test]
    fn ntp_time_is_counted_from_1900() {
        // The Unix epoch is 2208988800 s after the NTP one; half a second
        // is half of the fraction.
        let ntp = ntp_from(Duration::from_millis(500));
        assert_eq!(ntp >> 32, NTP_OFFSET);
        assert_eq!(ntp & 0xFFFF_FFFF, 1 << 31);
        assert_eq!(ntp_middle(ntp), ((NTP_OFFSET as u32) << 16) | 0x8000);
        assert!(ntp_now() >> 32 > NTP_OFFSET + 1_700_000_000);
    }

    #[test]
    fn reports_are_written_and_read_back() {
        let sender = SenderInfo {
            ntp: 0x0123_4567_89AB_CDEF,
            rtp_timestamp: 160_000,
            packets: 1_000,
            octets: 160_000,
        };
        let block = ReportBlock {
            ssrc: 0xAABB_CCDD,
            fraction_lost: 13,
            cumulative_lost: -2,
            highest_sequence: 70_000,
            jitter: 42,
            last_report: 0x4567_89AB,
            delay_since_last: 65_536,
        };
        let bytes = build_report(0x1122_3344, Some(sender), Some(block), "phone@pc");
        // Whole 32-bit words, and the first part is a sender report.
        assert_eq!(bytes.len() % 4, 0);
        assert_eq!(bytes[1], SENDER_REPORT);
        assert_eq!(
            parse(&bytes),
            Some(Report {
                ssrc: 0x1122_3344,
                sender: Some(sender),
                blocks: vec![block],
                bye: false
            })
        );

        let receiver_only = build_report(7, None, None, "");
        assert_eq!(receiver_only[1], RECEIVER_REPORT);
        assert_eq!(
            parse(&receiver_only),
            Some(Report {
                ssrc: 7,
                ..Report::default()
            })
        );
        assert!(parse(&build_bye(9)).unwrap().bye);
        // Cut short, or not RTCP.
        assert_eq!(parse(&bytes[..bytes.len() - 6]), None);
        assert_eq!(parse(b"not rtcp at all"), None);
    }

    fn reception_of(sequences: &[u16]) -> Reception {
        let mut reception = Reception::default();
        for sequence in sequences {
            let timestamp = u32::from(*sequence) * 160;
            reception.on_packet(5, *sequence, timestamp, timestamp + 1_000);
        }
        reception
    }

    #[test]
    fn losses_are_counted_per_interval_and_in_all() {
        // 10 sent, 2 lost.
        let mut reception = reception_of(&[0, 1, 2, 4, 5, 6, 7, 9]);
        let first = reception.report(0, 0).unwrap();
        assert_eq!(first.ssrc, 5);
        assert_eq!(first.highest_sequence, 9);
        assert_eq!(first.cumulative_lost, 2);
        assert_eq!(first.fraction_lost, (2 * 256 / 10) as u8);
        // Nothing lost since: the fraction is zero, the total stays.
        for sequence in 10..20 {
            reception.on_packet(
                5,
                sequence,
                u32::from(sequence) * 160,
                u32::from(sequence) * 160 + 1_000,
            );
        }
        let second = reception.report(0, 0).unwrap();
        assert_eq!(second.fraction_lost, 0);
        assert_eq!(second.cumulative_lost, 2);
        // A duplicate makes the total smaller, never the fraction negative.
        reception.on_packet(5, 19, 19 * 160, 19 * 160 + 1_000);
        let third = reception.report(0, 0).unwrap();
        assert_eq!(third.fraction_lost, 0);
        assert_eq!(third.cumulative_lost, 1);
        assert_eq!(Reception::default().report(0, 0), None);
    }

    #[test]
    fn sequence_numbers_that_wrap_are_counted_on() {
        let sequences: Vec<u16> = (65_530..=65_535).chain(0..5).collect();
        let mut reception = reception_of(&sequences);
        let block = reception.report(0, 0).unwrap();
        assert_eq!(block.highest_sequence, (1 << 16) + 4);
        assert_eq!(block.cumulative_lost, 0);
    }

    #[test]
    fn a_sender_that_restarts_is_followed_after_two_packets() {
        let mut reception = reception_of(&[100, 101, 102]);
        // One stray far number is not believed ...
        reception.on_packet(5, 40_000, 0, 1_000);
        assert_eq!(reception.report(0, 0).unwrap().highest_sequence, 102);
        // ... two in a row are.
        reception.on_packet(5, 40_001, 160, 1_160);
        reception.on_packet(5, 40_002, 320, 1_320);
        let block = reception.report(0, 0).unwrap();
        assert_eq!(block.highest_sequence, 40_002);
        assert_eq!(block.cumulative_lost, 0);
    }

    #[test]
    fn jitter_follows_uneven_arrival() {
        // Steady: no jitter.
        let mut steady = reception_of(&(0..100).collect::<Vec<u16>>());
        assert_eq!(steady.report(0, 0).unwrap().jitter, 0);
        // Every other packet 80 units (10 ms at 8 kHz) late: the jitter
        // settles near 80.
        let mut uneven = Reception::default();
        for sequence in 0..400_u16 {
            let timestamp = u32::from(sequence) * 160;
            let late = if sequence % 2 == 0 { 0 } else { 80 };
            uneven.on_packet(5, sequence, timestamp, timestamp + 1_000 + late);
        }
        let jitter = uneven.report(0, 0).unwrap().jitter;
        assert!((70..=85).contains(&jitter), "jitter {jitter}");
    }

    #[test]
    fn the_other_sides_report_reads_as_loss_jitter_and_delay() {
        let sent_at = ntp_from(Duration::from_secs(1_000));
        let now = ntp_from(Duration::from_millis(1_000_250));
        // The other side held our report for 100 ms before answering:
        // the round trip is the remaining 150 ms.
        let block = ReportBlock {
            ssrc: 1,
            fraction_lost: 64,
            cumulative_lost: 30,
            highest_sequence: 1_000,
            jitter: 160,
            last_report: ntp_middle(sent_at),
            delay_since_last: 65_536 / 10,
        };
        let quality = RemoteQuality::from_block(&block, 8_000, now);
        assert_eq!(quality.lost_percent, 25.0);
        assert_eq!(quality.lost_total, 30);
        assert_eq!(quality.jitter_ms, 20.0);
        let round_trip = quality.round_trip_ms.unwrap();
        assert!((round_trip - 150.0).abs() < 1.0, "{round_trip}");
        // No report of ours seen yet: no round trip.
        let none = ReportBlock {
            last_report: 0,
            ..block
        };
        assert_eq!(
            RemoteQuality::from_block(&none, 8_000, now).round_trip_ms,
            None
        );
    }
}
