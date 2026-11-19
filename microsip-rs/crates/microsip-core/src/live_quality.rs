//! The call-quality lamp: green, yellow or red while a call is connected,
//! from what the last few seconds were like.
//!
//! Every couple of seconds the counters of the call's audio are compared
//! with the previous look: how many packets came and how many never did,
//! how often playback ran dry, and what the other side last reported
//! (RTCP) about this phone's own audio: its loss, how unevenly the packets
//! arrived (jitter) and the time there and back.

use crate::call_media::{MediaQuality, MediaStats, RemoteSummary};

/// Colour of the lamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Lamp {
    /// Clear speech.
    #[default]
    Good,
    /// Noticeable: words may break up now and then.
    Fair,
    /// Hard to talk: much is lost, or nothing arrives at all.
    Poor,
}

/// What the last stretch of a call was like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveQuality {
    pub lamp: Lamp,
    /// Share of the packets meant for this phone that never came, in
    /// percent, over the last stretch.
    pub incoming_loss_percent: u32,
    /// Packets that came over the last stretch.
    pub received: u64,
    /// Times playback ran dry over the last stretch.
    pub gaps: u64,
    /// What the other side last said about this phone's audio.
    pub remote: Option<RemoteSummary>,
}

/// Loss at which speech starts to break up (percent).
const FAIR_LOSS: u32 = 2;
/// Loss at which talking gets hard (percent).
const POOR_LOSS: u32 = 8;
/// Unevenness the playback reserve no longer hides well (ms).
const FAIR_JITTER: u32 = 40;
const POOR_JITTER: u32 = 100;
/// Delay there and back at which people start talking over each other (ms).
const FAIR_ROUND_TRIP: u32 = 400;
const POOR_ROUND_TRIP: u32 = 800;
/// Times playback may run dry in one stretch.
const FAIR_GAPS: u64 = 3;
const POOR_GAPS: u64 = 10;

/// Follows one call; give it a fresh one for each call or after hold.
#[derive(Debug, Default)]
pub struct QualityMeter {
    previous: Option<(MediaQuality, MediaStats)>,
}

impl QualityMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the previous look: after hold the counts start over.
    pub fn reset(&mut self) {
        self.previous = None;
    }

    /// Takes the counters as they are now and tells what the stretch since
    /// the previous look was like. The first look covers the call so far.
    pub fn look(&mut self, quality: MediaQuality, stats: MediaStats) -> LiveQuality {
        let (before, before_stats) = self.previous.unwrap_or_default();
        self.previous = Some((quality, stats));
        let received = stats
            .received_packets
            .saturating_sub(before_stats.received_packets);
        let lost = quality.lost.saturating_sub(before.lost);
        let gaps = quality.gaps.saturating_sub(before.gaps);
        let expected = received + lost;
        // Rounded to the nearest percent; nothing expected, nothing lost.
        let incoming_loss_percent = (lost * 100 + expected / 2)
            .checked_div(expected)
            .unwrap_or(0) as u32;
        let mut live = LiveQuality {
            lamp: Lamp::Good,
            incoming_loss_percent,
            received,
            gaps,
            remote: quality.remote,
        };
        live.lamp = lamp_for(&live);
        live
    }
}

/// The lamp for a stretch: the worst of what was measured.
fn lamp_for(live: &LiveQuality) -> Lamp {
    // Nothing at all arrived: the other side cannot be heard.
    if live.received == 0 {
        return Lamp::Poor;
    }
    let mut lamp =
        grade(live.incoming_loss_percent, FAIR_LOSS, POOR_LOSS).max(grade_gaps(live.gaps));
    if let Some(remote) = live.remote {
        lamp = lamp
            .max(grade(remote.lost_percent, FAIR_LOSS, POOR_LOSS))
            .max(grade(remote.jitter_ms, FAIR_JITTER, POOR_JITTER));
        if let Some(round_trip) = remote.round_trip_ms {
            lamp = lamp.max(grade(round_trip, FAIR_ROUND_TRIP, POOR_ROUND_TRIP));
        }
    }
    lamp
}

fn grade(value: u32, fair: u32, poor: u32) -> Lamp {
    if value >= poor {
        Lamp::Poor
    } else if value >= fair {
        Lamp::Fair
    } else {
        Lamp::Good
    }
}

fn grade_gaps(gaps: u64) -> Lamp {
    if gaps >= POOR_GAPS {
        Lamp::Poor
    } else if gaps >= FAIR_GAPS {
        Lamp::Fair
    } else {
        Lamp::Good
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(received: u64, lost: u64, gaps: u64) -> (MediaQuality, MediaStats) {
        (
            MediaQuality {
                lost,
                gaps,
                ..MediaQuality::default()
            },
            MediaStats {
                sent_packets: received,
                received_packets: received,
            },
        )
    }

    fn remote(lost_percent: u32, jitter_ms: u32, round_trip_ms: Option<u32>) -> RemoteSummary {
        RemoteSummary {
            lost_percent,
            lost_total: 0,
            jitter_ms,
            round_trip_ms,
        }
    }

    #[test]
    fn a_clean_call_is_green() {
        let mut meter = QualityMeter::new();
        let (quality, stats) = counters(100, 0, 0);
        let live = meter.look(quality, stats);
        assert_eq!(live.lamp, Lamp::Good);
        assert_eq!((live.received, live.incoming_loss_percent), (100, 0));
    }

    #[test]
    fn only_the_last_stretch_counts() {
        let mut meter = QualityMeter::new();
        // A bad start: 20 of 120 lost.
        let (quality, stats) = counters(100, 20, 0);
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Poor);
        // Then 100 more arrive and nothing more is lost: green again,
        // although the call as a whole lost 10 %.
        let (quality, stats) = counters(200, 20, 0);
        let live = meter.look(quality, stats);
        assert_eq!(live.incoming_loss_percent, 0);
        assert_eq!(live.lamp, Lamp::Good);
        // 3 of the next 100 lost: yellow. 10 of 100: red.
        let (quality, stats) = counters(297, 23, 0);
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Fair);
        let (quality, stats) = counters(387, 33, 0);
        let live = meter.look(quality, stats);
        assert_eq!(live.incoming_loss_percent, 10);
        assert_eq!(live.lamp, Lamp::Poor);
    }

    #[test]
    fn silence_from_the_other_side_is_red() {
        let mut meter = QualityMeter::new();
        let (quality, stats) = counters(100, 0, 0);
        meter.look(quality, stats);
        // Nothing new arrived since the previous look.
        let live = meter.look(quality, stats);
        assert_eq!(live.received, 0);
        assert_eq!(live.lamp, Lamp::Poor);
        // After hold the meter starts over and the old count is no excuse.
        meter.reset();
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Good);
    }

    #[test]
    fn playback_running_dry_turns_the_lamp() {
        let mut meter = QualityMeter::new();
        let (quality, stats) = counters(100, 0, 2);
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Good);
        let (quality, stats) = counters(200, 0, 6);
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Fair);
        let (quality, stats) = counters(300, 0, 20);
        assert_eq!(meter.look(quality, stats).lamp, Lamp::Poor);
    }

    #[test]
    fn what_the_other_side_reports_counts_too() {
        for (report, lamp) in [
            (remote(0, 5, Some(30)), Lamp::Good),
            (remote(3, 5, Some(30)), Lamp::Fair),
            (remote(12, 5, None), Lamp::Poor),
            (remote(0, 60, None), Lamp::Fair),
            (remote(0, 150, None), Lamp::Poor),
            (remote(0, 5, Some(500)), Lamp::Fair),
            (remote(0, 5, Some(900)), Lamp::Poor),
        ] {
            let mut meter = QualityMeter::new();
            let (mut quality, stats) = counters(100, 0, 0);
            quality.remote = Some(report);
            let live = meter.look(quality, stats);
            assert_eq!(live.lamp, lamp, "{report:?}");
            assert_eq!(live.remote, Some(report));
        }
    }
}
