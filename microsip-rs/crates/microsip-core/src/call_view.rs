//! Interface-side view of a call, driven by executor events.
//!
//! Keeps the state a window needs (which buttons are allowed, what to show)
//! separate from the drawing code so it can be tested without a window.

use crate::call_runner::{EndReason, Event};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// INVITE sent, nothing heard yet.
    Calling,
    /// The remote phone is ringing.
    Ringing,
    /// An incoming call waits for the user.
    Incoming,
    /// The user answered; waiting for the caller's ACK.
    Answering,
    Connected,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Calling => "Calling",
            Self::Ringing => "Remote is ringing",
            Self::Incoming => "Incoming call",
            Self::Answering => "Answering",
            Self::Connected => "Connected",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CallView {
    phase: Phase,
    peer: Option<SocketAddr>,
    stats: Option<(u64, u64)>,
    last_end: Option<EndReason>,
}

impl CallView {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    /// RTP packets (sent, received) of the call that just ended.
    pub fn stats(&self) -> Option<(u64, u64)> {
        self.stats
    }

    pub fn last_end(&self) -> Option<EndReason> {
        self.last_end
    }

    pub fn can_dial(&self) -> bool {
        self.phase == Phase::Idle
    }

    pub fn can_answer(&self) -> bool {
        self.phase == Phase::Incoming
    }

    pub fn can_reject(&self) -> bool {
        self.phase == Phase::Incoming
    }

    /// Hang-up is not offered while answering: it would be refused until the
    /// caller's ACK arrives.
    pub fn can_hangup(&self) -> bool {
        matches!(
            self.phase,
            Phase::Calling | Phase::Ringing | Phase::Connected
        )
    }

    /// Call right after sending `UiCommand::Dial`.
    pub fn dialing(&mut self, peer: SocketAddr) {
        if self.can_dial() {
            self.phase = Phase::Calling;
            self.peer = Some(peer);
            self.stats = None;
            self.last_end = None;
        }
    }

    /// Call right after sending `UiCommand::Answer`.
    pub fn answering(&mut self) {
        if self.can_answer() {
            self.phase = Phase::Answering;
        }
    }

    pub fn on_event(&mut self, event: &Event) {
        match event {
            Event::Incoming { peer } => {
                self.phase = Phase::Incoming;
                self.peer = Some(*peer);
                self.stats = None;
                self.last_end = None;
            }
            Event::Ringing => {
                if self.phase == Phase::Calling {
                    self.phase = Phase::Ringing;
                }
            }
            Event::Established => self.phase = Phase::Connected,
            Event::MediaStats { sent, received } => self.stats = Some((*sent, *received)),
            Event::Ended(reason) => {
                self.phase = Phase::Idle;
                self.last_end = Some(*reason);
            }
            Event::Error(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> SocketAddr {
        "127.0.0.1:5070".parse().unwrap()
    }

    #[test]
    fn outgoing_call_walks_through_its_phases() {
        let mut view = CallView::default();
        assert!(view.can_dial());
        assert!(!view.can_hangup());

        view.dialing(peer());
        assert_eq!(view.phase(), Phase::Calling);
        assert!(view.can_hangup());
        assert!(!view.can_dial());

        view.on_event(&Event::Ringing);
        assert_eq!(view.phase(), Phase::Ringing);
        view.on_event(&Event::Established);
        assert_eq!(view.phase(), Phase::Connected);
        view.on_event(&Event::MediaStats {
            sent: 10,
            received: 12,
        });
        view.on_event(&Event::Ended(EndReason::Completed));

        assert_eq!(view.phase(), Phase::Idle);
        assert_eq!(view.stats(), Some((10, 12)));
        assert_eq!(view.last_end(), Some(EndReason::Completed));
        assert_eq!(view.peer(), Some(peer()));
    }

    #[test]
    fn incoming_call_offers_answer_and_reject_but_not_hangup() {
        let mut view = CallView::default();
        view.on_event(&Event::Incoming { peer: peer() });
        assert!(view.can_answer());
        assert!(view.can_reject());
        assert!(!view.can_hangup());
        assert!(!view.can_dial());

        view.answering();
        assert_eq!(view.phase(), Phase::Answering);
        assert!(!view.can_answer());
        assert!(!view.can_hangup());

        view.on_event(&Event::Established);
        assert!(view.can_hangup());
    }

    #[test]
    fn failed_dial_returns_to_idle_and_clears_old_statistics() {
        let mut view = CallView::default();
        view.on_event(&Event::MediaStats {
            sent: 1,
            received: 1,
        });
        view.dialing(peer());
        assert_eq!(view.stats(), None);

        view.on_event(&Event::Error("invalid destination".to_owned()));
        assert_eq!(view.phase(), Phase::Calling);
        view.on_event(&Event::Ended(EndReason::Failed));
        assert_eq!(view.phase(), Phase::Idle);
        assert_eq!(view.last_end(), Some(EndReason::Failed));
        assert!(view.can_dial());
    }

    #[test]
    fn dialing_is_ignored_while_a_call_exists() {
        let mut view = CallView::default();
        view.on_event(&Event::Incoming { peer: peer() });
        view.dialing("127.0.0.1:6000".parse().unwrap());
        assert_eq!(view.phase(), Phase::Incoming);
        assert_eq!(view.peer(), Some(peer()));
    }

    #[test]
    fn ringing_is_ignored_unless_calling() {
        let mut view = CallView::default();
        view.on_event(&Event::Ringing);
        assert_eq!(view.phase(), Phase::Idle);
    }
}
