//! Pure call state machine for one SIP call.
//!
//! The engine performs no I/O. It receives [`Input`] values (user commands or
//! already classified SIP events) and returns the [`Action`] list that an
//! executor must carry out, in order. Message construction stays in
//! `invite_control`, `dialog` and `transaction_headers`; sockets and audio stay
//! in the executor.
//!
//! Callers must classify traffic before it reaches the engine: a
//! retransmission of the current INVITE or a second new INVITE is not an
//! [`Input::IncomingInvite`] for an engine that is already busy. Retained
//! transactions (retransmitted answers, repeated ACKs) belong to the
//! transaction layer and are not modelled here. The engine has no timers: the
//! executor owns them and reports an expired one as [`Input::Timeout`].

use std::error::Error;
use std::fmt;

/// Status code sent when the local user rejects or hangs up an incoming call.
const BUSY_HERE: u16 = 486;
/// Status code sent for an INVITE that was cancelled before being answered.
const REQUEST_TERMINATED: u16 = 487;

/// Externally visible call state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// No call.
    #[default]
    Idle,
    /// Local INVITE sent, no final response yet (also while a CANCEL is pending).
    Outgoing,
    /// Incoming INVITE received, not answered yet.
    Incoming,
    /// 2xx sent for an incoming INVITE, waiting for the ACK.
    Answered,
    /// Dialog confirmed, media running.
    Established,
    /// Local BYE sent, waiting for its response.
    Ending,
}

/// Commands from the local user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Dial,
    Answer,
    Reject,
    Hangup,
}

/// Everything the engine can react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Command(Command),
    IncomingInvite,
    IncomingAck,
    IncomingBye,
    IncomingCancel,
    /// Response to our INVITE (not to CANCEL or BYE).
    InviteResponse(u16),
    /// Response to our BYE.
    ByeResponse(u16),
    /// The executor's retransmission timer gave up waiting for the peer.
    Timeout,
}

/// Work the executor must perform, in the order returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SendInvite,
    SendRinging,
    SendInviteSuccess,
    SendInviteFailure(u16),
    SendSuccessAck,
    SendFailureAck,
    SendCancel,
    SendCancelSuccess,
    SendBye,
    SendByeSuccess,
    StartMedia,
    StopMedia,
    ReportEstablished,
    ReportEnded,
}

/// An input that is not valid in the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineError {
    pub state: State,
    pub input: Input,
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "input {:?} is not valid in state {:?}",
            self.input, self.state
        )
    }
}

impl Error for EngineError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Cancel {
    #[default]
    NotRequested,
    /// User hung up before any provisional response; CANCEL must wait for one.
    Pending,
    Sent,
}

#[derive(Debug, Clone, Default)]
pub struct CallEngine {
    state: State,
    cancel: Cancel,
    /// A 1xx response was received for our INVITE.
    provisional_seen: bool,
}

impl CallEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn handle(&mut self, input: Input) -> Result<Vec<Action>, EngineError> {
        match (self.state, input) {
            (State::Idle, Input::Command(Command::Dial)) => {
                self.state = State::Outgoing;
                self.cancel = Cancel::NotRequested;
                self.provisional_seen = false;
                Ok(vec![Action::SendInvite])
            }
            (State::Idle, Input::IncomingInvite) => {
                self.state = State::Incoming;
                Ok(vec![Action::SendRinging])
            }

            (State::Outgoing, Input::Command(Command::Hangup)) => Ok(self.cancel_outgoing()),
            (State::Outgoing, Input::Timeout) => {
                self.state = State::Idle;
                Ok(vec![Action::ReportEnded])
            }
            (State::Outgoing, Input::InviteResponse(status)) => {
                self.outgoing_response(input, status)
            }

            (State::Incoming, Input::Command(Command::Answer)) => {
                self.state = State::Answered;
                Ok(vec![Action::SendInviteSuccess])
            }
            (State::Incoming, Input::Command(Command::Reject | Command::Hangup)) => {
                self.state = State::Idle;
                Ok(vec![
                    Action::SendInviteFailure(BUSY_HERE),
                    Action::ReportEnded,
                ])
            }
            (State::Incoming, Input::IncomingCancel) => {
                self.state = State::Idle;
                Ok(vec![
                    Action::SendCancelSuccess,
                    Action::SendInviteFailure(REQUEST_TERMINATED),
                    Action::ReportEnded,
                ])
            }

            (State::Answered, Input::IncomingAck) => {
                self.state = State::Established;
                Ok(vec![Action::StartMedia, Action::ReportEstablished])
            }
            // CANCEL arrived after the 2xx: it has no effect on the call.
            (State::Answered, Input::IncomingCancel) => Ok(vec![Action::SendCancelSuccess]),
            // No ACK for our 2xx within the retransmission window: end the session.
            (State::Answered, Input::Timeout) => {
                self.state = State::Ending;
                Ok(vec![Action::SendBye])
            }
            // BYE before the ACK: media never started.
            (State::Answered, Input::IncomingBye) => {
                self.state = State::Idle;
                Ok(vec![Action::SendByeSuccess, Action::ReportEnded])
            }

            (State::Established, Input::Command(Command::Hangup)) => {
                self.state = State::Ending;
                Ok(vec![Action::SendBye, Action::StopMedia])
            }
            (State::Established, Input::IncomingBye) => {
                self.state = State::Idle;
                Ok(vec![
                    Action::SendByeSuccess,
                    Action::StopMedia,
                    Action::ReportEnded,
                ])
            }
            // Duplicate ACK.
            (State::Established, Input::IncomingAck) => Ok(Vec::new()),
            // Retransmitted 2xx: acknowledge again.
            (State::Established, Input::InviteResponse(200..=299)) => {
                Ok(vec![Action::SendSuccessAck])
            }

            (State::Ending, Input::ByeResponse(status)) => match status {
                100..=199 => Ok(Vec::new()),
                200..=699 => {
                    self.state = State::Idle;
                    Ok(vec![Action::ReportEnded])
                }
                _ => Err(self.invalid(input)),
            },
            (State::Ending, Input::Timeout) => {
                self.state = State::Idle;
                Ok(vec![Action::ReportEnded])
            }
            // Both sides hung up at once: answer theirs, keep waiting for ours.
            (State::Ending, Input::IncomingBye) => Ok(vec![Action::SendByeSuccess]),
            // Retransmitted 2xx while our BYE is in flight.
            (State::Ending, Input::InviteResponse(200..=299)) => Ok(vec![Action::SendSuccessAck]),

            _ => Err(self.invalid(input)),
        }
    }

    fn invalid(&self, input: Input) -> EngineError {
        EngineError {
            state: self.state,
            input,
        }
    }

    /// Hangup while our INVITE is unanswered. A CANCEL may only be sent after a
    /// provisional response, so before that it is remembered and emitted later.
    fn cancel_outgoing(&mut self) -> Vec<Action> {
        match self.cancel {
            Cancel::NotRequested if self.provisional_seen => {
                self.cancel = Cancel::Sent;
                vec![Action::SendCancel]
            }
            Cancel::NotRequested => {
                self.cancel = Cancel::Pending;
                Vec::new()
            }
            Cancel::Pending | Cancel::Sent => Vec::new(),
        }
    }

    fn outgoing_response(&mut self, input: Input, status: u16) -> Result<Vec<Action>, EngineError> {
        match status {
            100..=199 => {
                self.provisional_seen = true;
                if self.cancel == Cancel::Pending {
                    self.cancel = Cancel::Sent;
                    Ok(vec![Action::SendCancel])
                } else {
                    Ok(Vec::new())
                }
            }
            200..=299 => {
                if self.cancel == Cancel::NotRequested {
                    self.state = State::Established;
                    Ok(vec![
                        Action::SendSuccessAck,
                        Action::StartMedia,
                        Action::ReportEstablished,
                    ])
                } else {
                    // The answer won the race with our CANCEL: ACK, then hang up.
                    self.state = State::Ending;
                    Ok(vec![Action::SendSuccessAck, Action::SendBye])
                }
            }
            300..=699 => {
                self.state = State::Idle;
                Ok(vec![Action::SendFailureAck, Action::ReportEnded])
            }
            _ => Err(self.invalid(input)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hangup_before_provisional_waits_for_one_before_cancel() {
        let mut engine = CallEngine::new();
        engine.handle(Input::Command(Command::Dial)).unwrap();
        assert!(
            engine
                .handle(Input::Command(Command::Hangup))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            engine.handle(Input::InviteResponse(100)).unwrap(),
            vec![Action::SendCancel]
        );
        assert_eq!(
            engine.handle(Input::InviteResponse(487)).unwrap(),
            vec![Action::SendFailureAck, Action::ReportEnded]
        );
        assert_eq!(engine.state(), State::Idle);
    }

    #[test]
    fn repeated_hangup_sends_one_cancel() {
        let mut engine = CallEngine::new();
        engine.handle(Input::Command(Command::Dial)).unwrap();
        engine.handle(Input::InviteResponse(180)).unwrap();
        engine.handle(Input::Command(Command::Hangup)).unwrap();
        engine.handle(Input::InviteResponse(180)).unwrap();
        assert!(
            engine
                .handle(Input::Command(Command::Hangup))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_input_is_an_error_and_keeps_state() {
        let mut engine = CallEngine::new();
        let error = engine.handle(Input::IncomingAck).unwrap_err();
        assert_eq!(error.state, State::Idle);
        assert_eq!(engine.state(), State::Idle);
    }

    #[test]
    fn timeout_ends_an_unanswered_outgoing_call() {
        let mut engine = CallEngine::new();
        engine.handle(Input::Command(Command::Dial)).unwrap();
        assert_eq!(
            engine.handle(Input::Timeout).unwrap(),
            vec![Action::ReportEnded]
        );
        assert_eq!(engine.state(), State::Idle);
    }

    #[test]
    fn missing_ack_ends_with_bye_and_bye_timeout_ends_call() {
        let mut engine = CallEngine::new();
        engine.handle(Input::IncomingInvite).unwrap();
        engine.handle(Input::Command(Command::Answer)).unwrap();
        assert_eq!(
            engine.handle(Input::Timeout).unwrap(),
            vec![Action::SendBye]
        );
        assert_eq!(engine.state(), State::Ending);
        assert_eq!(
            engine.handle(Input::Timeout).unwrap(),
            vec![Action::ReportEnded]
        );
        assert_eq!(engine.state(), State::Idle);
    }

    #[test]
    fn timeout_is_invalid_when_nothing_is_pending() {
        let mut engine = CallEngine::new();
        assert!(engine.handle(Input::Timeout).is_err());
    }

    #[test]
    fn busy_engine_rejects_a_second_dial() {
        let mut engine = CallEngine::new();
        engine.handle(Input::Command(Command::Dial)).unwrap();
        assert!(engine.handle(Input::Command(Command::Dial)).is_err());
        assert_eq!(engine.state(), State::Outgoing);
    }

    #[test]
    fn duplicate_success_is_acknowledged_again() {
        let mut engine = CallEngine::new();
        engine.handle(Input::Command(Command::Dial)).unwrap();
        engine.handle(Input::InviteResponse(200)).unwrap();
        assert_eq!(
            engine.handle(Input::InviteResponse(200)).unwrap(),
            vec![Action::SendSuccessAck]
        );
        assert_eq!(engine.state(), State::Established);
    }
}
