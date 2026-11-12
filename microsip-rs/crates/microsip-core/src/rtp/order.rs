//! Puts received voice packets back into sending order.
//!
//! A network, and a VPN in particular, may deliver packets out of order,
//! twice, or not at all. Played as they arrive, swapped or repeated packets
//! sound like crackling. [`PacketOrderer`] releases packets strictly by RTP
//! sequence number: an early packet waits briefly for the missing one, a
//! duplicate or a packet that arrives after its turn is dropped, and a packet
//! that never comes is reported as lost so the gap can be concealed.

use std::collections::BTreeMap;

/// Packets held back while waiting for a missing one. With one, a packet
/// that is overtaken by its successor is still put back in place, and a
/// lost packet stalls the audio for a single packet time. Waiting for two
/// (as this first did) stalled it for 80 ms with the 40 ms packets some
/// servers send, which on a lossy link emptied the playback reserve at
/// every loss; measured on a real call as "gaps".
const MAX_HELD: usize = 1;

/// A jump this far ahead is a new stream (for example after a transfer),
/// not a gap to wait for.
const RESTART_GAP: u16 = 100;

/// So many stale packets in a row mean the sender restarted its numbering.
const STALE_RUN: u32 = 10;

#[derive(Debug, Clone, PartialEq)]
pub enum Released<T> {
    Packet(T),
    /// One packet is missing at this place.
    Lost,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderStats {
    /// Packets that never arrived in time.
    pub lost: u64,
    /// Packets that arrived out of order and were put back in place.
    pub reordered: u64,
    /// Duplicates and packets that arrived after their turn.
    pub discarded: u64,
}

#[derive(Debug)]
pub struct PacketOrderer<T> {
    ssrc: Option<u32>,
    /// Sequence number expected next.
    next: u16,
    /// Early packets, keyed by their distance ahead of `next`.
    held: BTreeMap<u16, T>,
    stale_run: u32,
    stats: OrderStats,
}

impl<T> Default for PacketOrderer<T> {
    fn default() -> Self {
        Self {
            ssrc: None,
            next: 0,
            held: BTreeMap::new(),
            stale_run: 0,
            stats: OrderStats::default(),
        }
    }
}

impl<T> PacketOrderer<T> {
    pub fn stats(&self) -> OrderStats {
        self.stats
    }

    /// Takes a received packet and returns what may be played now, in order.
    pub fn push(&mut self, ssrc: u32, sequence: u16, packet: T) -> Vec<Released<T>> {
        let mut released = Vec::new();
        if self.ssrc != Some(ssrc) {
            // A different sender: its numbering is unrelated to the old one.
            self.restart(ssrc, sequence, &mut released);
        }

        let mut ahead = sequence.wrapping_sub(self.next);
        if ahead >= 0x8000 {
            // Behind the play position: a duplicate or too late to use.
            self.stats.discarded += 1;
            self.stale_run += 1;
            if self.stale_run < STALE_RUN {
                return released;
            }
            self.restart(ssrc, sequence, &mut released);
            ahead = 0;
        } else if ahead >= RESTART_GAP {
            self.restart(ssrc, sequence, &mut released);
            ahead = 0;
        }
        self.stale_run = 0;

        if self.held.contains_key(&ahead) {
            self.stats.discarded += 1;
            return released;
        }
        // A packet with a later number arrived first.
        if self
            .held
            .keys()
            .next_back()
            .is_some_and(|last| *last > ahead)
        {
            self.stats.reordered += 1;
        }
        self.held.insert(ahead, packet);

        loop {
            if let Some(packet) = self.held.remove(&0) {
                released.push(Released::Packet(packet));
            } else if self.held.len() > MAX_HELD {
                // Waited long enough: play on without the missing packet.
                self.stats.lost += 1;
                released.push(Released::Lost);
            } else {
                break;
            }
            self.advance();
        }
        released
    }

    /// Moves the play position one packet forward.
    fn advance(&mut self) {
        self.next = self.next.wrapping_add(1);
        self.held = std::mem::take(&mut self.held)
            .into_iter()
            .map(|(ahead, packet)| (ahead - 1, packet))
            .collect();
    }

    fn restart(&mut self, ssrc: u32, sequence: u16, released: &mut Vec<Released<T>>) {
        // What was still waiting belongs to the old stream; play it out.
        released.extend(
            std::mem::take(&mut self.held)
                .into_values()
                .map(Released::Packet),
        );
        self.ssrc = Some(ssrc);
        self.next = sequence;
        self.stale_run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SSRC: u32 = 7;

    /// Pushes sequence numbers and returns what was released: the number for
    /// a packet, `None` for a reported loss.
    fn run(orderer: &mut PacketOrderer<u16>, sequences: &[u16]) -> Vec<Option<u16>> {
        sequences
            .iter()
            .flat_map(|sequence| orderer.push(SSRC, *sequence, *sequence))
            .map(|released| match released {
                Released::Packet(sequence) => Some(sequence),
                Released::Lost => None,
            })
            .collect()
    }

    fn packets(sequences: &[u16]) -> Vec<Option<u16>> {
        sequences.iter().copied().map(Some).collect()
    }

    #[test]
    fn packets_in_order_pass_straight_through() {
        let mut orderer = PacketOrderer::default();
        for sequence in [500_u16, 501, 502] {
            assert_eq!(run(&mut orderer, &[sequence]), packets(&[sequence]));
        }
        assert_eq!(orderer.stats(), OrderStats::default());
    }

    #[test]
    fn swapped_packets_are_put_back_in_order() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[10]), packets(&[10]));
        // 12 arrives before 11: it waits, then both go out in order.
        assert_eq!(run(&mut orderer, &[12]), packets(&[]));
        assert_eq!(run(&mut orderer, &[11]), packets(&[11, 12]));
        assert_eq!(run(&mut orderer, &[13]), packets(&[13]));
        assert_eq!(orderer.stats().lost, 0);
        assert_eq!(orderer.stats().discarded, 0);
    }

    #[test]
    fn packet_overtaken_by_two_others_is_given_up() {
        // The wait is one packet: when 4 arrives and 2 is still missing,
        // play goes on without it, and 2 is dropped when it finally comes.
        let mut orderer = PacketOrderer::default();
        assert_eq!(
            run(&mut orderer, &[1, 3, 4, 2, 5]),
            vec![Some(1), None, Some(3), Some(4), Some(5)]
        );
        assert_eq!(orderer.stats().lost, 1);
        assert_eq!(orderer.stats().discarded, 1);
    }

    #[test]
    fn packet_overtaken_by_one_other_counts_as_reordered() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[1, 3, 2, 4]), packets(&[1, 2, 3, 4]));
        assert_eq!(orderer.stats().lost, 0);
        assert_eq!(orderer.stats().reordered, 1);
    }

    #[test]
    fn missing_packet_is_reported_lost_after_a_short_wait() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[1]), packets(&[1]));
        // 2 never comes. The next packet waits for it, the one after gives up.
        assert_eq!(run(&mut orderer, &[3]), packets(&[]));
        assert_eq!(run(&mut orderer, &[4]), vec![None, Some(3), Some(4)]);
        assert_eq!(run(&mut orderer, &[5]), packets(&[5]));
        assert_eq!(orderer.stats().lost, 1);

        // If 2 turns up now it is too late and must not be played.
        assert_eq!(run(&mut orderer, &[2]), packets(&[]));
        assert_eq!(orderer.stats().discarded, 1);
        assert_eq!(run(&mut orderer, &[6]), packets(&[6]));
    }

    #[test]
    fn duplicates_are_played_once() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[1, 1, 2, 2, 2, 3]), packets(&[1, 2, 3]));
        assert_eq!(orderer.stats().discarded, 3);

        // A duplicate of a packet that is still waiting.
        assert_eq!(run(&mut orderer, &[5, 5]), packets(&[]));
        assert_eq!(run(&mut orderer, &[4]), packets(&[4, 5]));
        assert_eq!(orderer.stats().discarded, 4);
    }

    #[test]
    fn sequence_numbers_wrap_around() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(
            run(&mut orderer, &[65_534, 0, 65_535, 1]),
            packets(&[65_534, 65_535, 0, 1])
        );
        assert_eq!(orderer.stats().lost, 0);
    }

    #[test]
    fn far_jump_starts_a_new_stream_without_reporting_losses() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[1, 2]), packets(&[1, 2]));
        assert_eq!(run(&mut orderer, &[5_000, 5_001]), packets(&[5_000, 5_001]));
        assert_eq!(orderer.stats().lost, 0);
    }

    #[test]
    fn new_sender_is_followed_even_when_its_numbers_are_lower() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[1_000, 1_002]), packets(&[1_000]));
        // Another SSRC: the waiting packet is played out, then the new stream.
        let released = orderer.push(SSRC + 1, 3, 3);
        assert_eq!(released, vec![Released::Packet(1_002), Released::Packet(3)]);
        assert_eq!(orderer.push(SSRC + 1, 4, 4), vec![Released::Packet(4)]);
        assert_eq!(orderer.stats().discarded, 0);
    }

    #[test]
    fn sender_that_restarts_its_numbering_is_followed_after_a_few_packets() {
        let mut orderer = PacketOrderer::default();
        assert_eq!(run(&mut orderer, &[30_000]), packets(&[30_000]));
        // Same SSRC, numbering starts again far below.
        let restarted: Vec<u16> = (100..100 + STALE_RUN as u16).collect();
        let released = run(&mut orderer, &restarted);
        assert_eq!(released, packets(&[*restarted.last().unwrap()]));
        assert_eq!(run(&mut orderer, &[110, 111]), packets(&[110, 111]));
    }
}
