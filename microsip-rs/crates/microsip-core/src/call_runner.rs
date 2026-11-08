//! Network executor for [`CallEngine`] over direct UDP.
//!
//! One thread owns the SIP socket, the engine and the media backend. The user
//! interface talks to it through [`CallHandle`]: commands go in, [`Event`]s
//! come out. Incoming datagrams are classified here, before the engine, so a
//! retransmitted INVITE or a stray request never becomes an engine input.
//!
//! Scope: direct UDP, one call, no proxy routing (no Route/Record-Route), no
//! registration. Retransmission timers follow the RFC 3261 doubling scheme
//! with fixed constants; the older `transaction` module is not used here.

use crate::call_engine::{Action, CallEngine, Command, Input, State};
use crate::call_media::Media;
use crate::dialog::{DialogConfig, EstablishedDialog};
use crate::invite_control::{build_cancel, build_failure_ack};
use crate::sip::message::{Headers, InviteRequest, Message, Request, Response, ResponseBuilder};
use crate::sip::sdp::{MediaKind, Sdp};
use crate::transaction_headers::TransactionHeaders;
use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type BoxError = Box<dyn Error + Send + Sync>;
type Result<T> = std::result::Result<T, BoxError>;

const TICK: Duration = Duration::from_millis(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Commands from the user interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    Dial(SocketAddr),
    Answer,
    Reject,
    Hangup,
    Shutdown,
}

/// Why a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Normal BYE in either direction.
    Completed,
    /// Final failure response, sent or received.
    Rejected(u16),
    /// Cancelled before it was answered.
    Cancelled,
    /// The peer stopped answering.
    Timeout,
    /// The call could not be started (bad destination, no audio port).
    Failed,
}

impl std::fmt::Display for EndReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => formatter.write_str("completed"),
            Self::Rejected(status) => write!(formatter, "rejected ({status})"),
            Self::Cancelled => formatter.write_str("cancelled"),
            Self::Timeout => formatter.write_str("timeout"),
            Self::Failed => formatter.write_str("failed"),
        }
    }
}

/// Notifications for the user interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Incoming {
        peer: SocketAddr,
    },
    Ringing,
    Established,
    /// RTP packet counters, sent just before the media stops.
    MediaStats {
        sent: u64,
        received: u64,
    },
    Ended(EndReason),
    Error(String),
}

/// Retransmission constants (RFC 3261 T1, T2 and 64*T1).
#[derive(Debug, Clone, Copy)]
pub struct Timers {
    pub t1: Duration,
    pub t2: Duration,
    pub give_up: Duration,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            t1: Duration::from_millis(500),
            t2: Duration::from_secs(4),
            give_up: Duration::from_secs(32),
        }
    }
}

/// Handle to a running executor thread. Dropping it shuts the call down.
pub struct CallHandle {
    commands: Sender<UiCommand>,
    events: Receiver<Event>,
    local: SocketAddr,
    thread: Option<JoinHandle<()>>,
}

impl CallHandle {
    pub fn local_address(&self) -> SocketAddr {
        self.local
    }

    /// Returns false when the executor has already stopped.
    pub fn send(&self, command: UiCommand) -> bool {
        self.commands.send(command).is_ok()
    }

    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub fn wait_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }
}

impl Drop for CallHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(UiCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts an executor with the default timers.
pub fn spawn<M, F>(local: SocketAddr, user: &str, make_media: F) -> io::Result<CallHandle>
where
    M: Media + 'static,
    F: FnOnce() -> M + Send + 'static,
{
    spawn_with_timers(local, user, Timers::default(), make_media)
}

/// Starts an executor. `make_media` runs on the new thread because audio
/// streams are not `Send`.
pub fn spawn_with_timers<M, F>(
    local: SocketAddr,
    user: &str,
    timers: Timers,
    make_media: F,
) -> io::Result<CallHandle>
where
    M: Media + 'static,
    F: FnOnce() -> M + Send + 'static,
{
    if local.ip().is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "specify a concrete local IP address, not 0.0.0.0",
        ));
    }
    let socket = UdpSocket::bind(local)?;
    socket.set_read_timeout(Some(TICK))?;
    let local = socket.local_addr()?;
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let user = user.to_owned();

    let thread = thread::spawn(move || {
        let runner = Runner::new(socket, local, user, timers, make_media(), event_sender);
        runner.run(&command_receiver);
    });

    Ok(CallHandle {
        commands: command_sender,
        events: event_receiver,
        local,
        thread: Some(thread),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetxKind {
    Invite,
    Cancel,
    Bye,
    Final,
}

struct Retx {
    kind: RetxKind,
    call_id: String,
    bytes: Vec<u8>,
    peer: SocketAddr,
    interval: Duration,
    cap: Duration,
    next: Instant,
    deadline: Instant,
}

/// Last response sent for a request, kept to answer retransmissions.
struct Retained {
    key: String,
    bytes: Vec<u8>,
    peer: SocketAddr,
    expires: Instant,
}

struct Call {
    peer: SocketAddr,
    call_id: String,
    invite: Request,
    invite_headers: TransactionHeaders,
    local_rtp: SocketAddr,
    remote_rtp: Option<SocketAddr>,
    local_tag: String,
    dialog: Option<EstablishedDialog>,
    last_invite_response: Option<Response>,
    ack_bytes: Option<Vec<u8>>,
    failure_ack: Option<Vec<u8>>,
    cancel_request: Option<Request>,
    cancel_headers: Option<TransactionHeaders>,
    incoming_cancel: Option<Request>,
    bye_request: Option<Request>,
    bye_headers: Option<TransactionHeaders>,
    incoming_bye: Option<Request>,
    reason: Option<EndReason>,
    ringing_reported: bool,
}

impl Call {
    fn new(
        peer: SocketAddr,
        invite: Request,
        local_rtp: SocketAddr,
        local_tag: String,
    ) -> Result<Self> {
        let invite_headers = TransactionHeaders::from_message(&Message::Request(invite.clone()))?;
        Ok(Self {
            peer,
            call_id: invite_headers.call_id.clone(),
            invite,
            invite_headers,
            local_rtp,
            remote_rtp: None,
            local_tag,
            dialog: None,
            last_invite_response: None,
            ack_bytes: None,
            failure_ack: None,
            cancel_request: None,
            cancel_headers: None,
            incoming_cancel: None,
            bye_request: None,
            bye_headers: None,
            incoming_bye: None,
            reason: None,
            ringing_reported: false,
        })
    }
}

struct Runner<M: Media> {
    socket: UdpSocket,
    local: SocketAddr,
    user: String,
    timers: Timers,
    media: M,
    engine: CallEngine,
    events: Sender<Event>,
    call: Option<Call>,
    retx: Vec<Retx>,
    retained: Vec<Retained>,
    pending: VecDeque<Input>,
    media_running: bool,
    counter: u64,
    buffer: Vec<u8>,
}

impl<M: Media> Runner<M> {
    fn new(
        socket: UdpSocket,
        local: SocketAddr,
        user: String,
        timers: Timers,
        media: M,
        events: Sender<Event>,
    ) -> Self {
        Self {
            socket,
            local,
            user,
            timers,
            media,
            engine: CallEngine::new(),
            events,
            call: None,
            retx: Vec::new(),
            retained: Vec::new(),
            pending: VecDeque::new(),
            media_running: false,
            counter: 0,
            buffer: vec![0_u8; 65_535],
        }
    }

    fn run(mut self, commands: &Receiver<UiCommand>) {
        let mut shutdown: Option<Instant> = None;

        loop {
            loop {
                match commands.try_recv() {
                    Ok(UiCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                        if shutdown.is_none() {
                            shutdown = Some(Instant::now() + SHUTDOWN_GRACE);
                            self.begin_shutdown();
                        }
                        break;
                    }
                    Ok(command) => self.on_command(command),
                    Err(TryRecvError::Empty) => break,
                }
            }

            if let Err(error) = self.receive_one() {
                self.report(format!("SIP socket failed: {error}"));
                break;
            }
            self.pump_media();
            self.tick_timers();
            self.drain_pending();

            if let Some(deadline) = shutdown
                && (self.engine.state() == State::Idle || Instant::now() >= deadline)
            {
                break;
            }
        }
        self.media.stop();
    }

    fn begin_shutdown(&mut self) {
        let input = match self.engine.state() {
            State::Outgoing | State::Established => Input::Command(Command::Hangup),
            State::Incoming => Input::Command(Command::Reject),
            State::Idle | State::Answered | State::Ending => return,
        };
        self.dispatch_reporting(input);
    }

    fn on_command(&mut self, command: UiCommand) {
        let result = match command {
            UiCommand::Dial(peer) => {
                let was_idle = self.engine.state() == State::Idle;
                let result = self.dial(peer);
                if result.is_err() && was_idle && self.engine.state() == State::Idle {
                    // Tell the interface the attempt is over, not just why.
                    if let Err(error) = &result {
                        self.report(error.to_string());
                    }
                    self.emit(Event::Ended(EndReason::Failed));
                    return;
                }
                result
            }
            UiCommand::Answer => self.dispatch(Input::Command(Command::Answer)),
            UiCommand::Reject => self.dispatch(Input::Command(Command::Reject)),
            UiCommand::Hangup => self.dispatch(Input::Command(Command::Hangup)),
            UiCommand::Shutdown => Ok(()),
        };
        if let Err(error) = result {
            self.report(error.to_string());
        }
    }

    fn dispatch_reporting(&mut self, input: Input) {
        if let Err(error) = self.dispatch(input) {
            self.report(error.to_string());
        }
    }

    fn drain_pending(&mut self) {
        while let Some(input) = self.pending.pop_front() {
            self.dispatch_reporting(input);
        }
    }

    fn report(&self, message: String) {
        self.emit(Event::Error(message));
    }

    fn emit(&self, event: Event) {
        // The interface may have gone away; the executor then just finishes.
        let _ = self.events.send(event);
    }

    fn unique(&mut self) -> String {
        self.counter += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:x}-{nanos:x}-{:x}", std::process::id(), self.counter)
    }

    fn branch(&mut self) -> String {
        format!("z9hG4bK-{}", self.unique())
    }

    fn contact(&self) -> String {
        format!("<sip:{}@{}>", self.user, self.local)
    }

    // ----- engine plumbing -------------------------------------------------

    fn dispatch(&mut self, input: Input) -> Result<()> {
        self.note_reason(input);
        let actions = self.engine.handle(input)?;
        for action in actions {
            if let Err(error) = self.execute(action) {
                self.report(format!("{action:?} failed: {error}"));
                match action {
                    // Without a sent request nothing will ever time out.
                    Action::SendBye | Action::SendInvite => {
                        self.pending.push_back(Input::Timeout);
                    }
                    Action::SendSuccessAck => {
                        self.pending.push_back(Input::Command(Command::Hangup));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn note_reason(&mut self, input: Input) {
        let reason = match (self.engine.state(), input) {
            (State::Outgoing, Input::Command(Command::Hangup)) => Some(EndReason::Cancelled),
            (State::Outgoing, Input::InviteResponse(status)) if status >= 300 => {
                Some(EndReason::Rejected(status))
            }
            (State::Incoming, Input::Command(Command::Reject | Command::Hangup)) => {
                Some(EndReason::Rejected(486))
            }
            (State::Incoming, Input::IncomingCancel) => Some(EndReason::Cancelled),
            (_, Input::Timeout) => Some(EndReason::Timeout),
            (State::Established, Input::Command(Command::Hangup))
            | (_, Input::IncomingBye)
            | (State::Ending, Input::ByeResponse(_)) => Some(EndReason::Completed),
            _ => None,
        };
        if let (Some(reason), Some(call)) = (reason, self.call.as_mut())
            && call.reason.is_none()
        {
            call.reason = Some(reason);
        }
    }

    fn call_ref(&self) -> Result<&Call> {
        self.call.as_ref().ok_or_else(|| "no current call".into())
    }

    fn call_mut(&mut self) -> Result<&mut Call> {
        self.call.as_mut().ok_or_else(|| "no current call".into())
    }

    fn execute(&mut self, action: Action) -> Result<()> {
        match action {
            Action::SendInvite => {
                let call = self.call_ref()?;
                let bytes = call.invite.serialize()?;
                let (peer, id) = (call.peer, call.call_id.clone());
                self.send(&bytes, peer)?;
                self.start_retx(RetxKind::Invite, id, bytes, peer, self.timers.give_up);
            }
            Action::SendRinging => {
                let bytes = self.invite_response(180, "Ringing", None)?;
                self.send_to_peer(&bytes)?;
            }
            Action::SendInviteSuccess => {
                let sdp = make_sdp(self.call_ref()?.local_rtp);
                let bytes = self.invite_response(200, "OK", Some(sdp))?;
                self.send_to_peer(&bytes)?;
                self.start_final_retx(bytes)?;
            }
            Action::SendInviteFailure(status) => {
                let bytes = self.invite_response(status, reason_phrase(status), None)?;
                self.send_to_peer(&bytes)?;
                self.start_final_retx(bytes)?;
            }
            Action::SendFailureAck => {
                let call = self.call_ref()?;
                let response = call
                    .last_invite_response
                    .as_ref()
                    .ok_or("no INVITE response to acknowledge")?;
                let bytes = build_failure_ack(&call.invite, response)?.serialize()?;
                let peer = call.peer;
                self.send(&bytes, peer)?;
                self.call_mut()?.failure_ack = Some(bytes);
            }
            Action::SendSuccessAck => self.send_success_ack()?,
            Action::SendCancel => {
                let request = build_cancel(&self.call_ref()?.invite)?;
                let headers = TransactionHeaders::from_message(&Message::Request(request.clone()))?;
                let bytes = request.serialize()?;
                let (peer, id) = {
                    let call = self.call_mut()?;
                    call.cancel_request = Some(request);
                    call.cancel_headers = Some(headers);
                    (call.peer, call.call_id.clone())
                };
                self.send(&bytes, peer)?;
                self.start_retx(RetxKind::Cancel, id, bytes, peer, self.timers.t2);
            }
            Action::SendCancelSuccess => {
                let call = self.call_ref()?;
                let request = call
                    .incoming_cancel
                    .clone()
                    .ok_or("no incoming CANCEL to answer")?;
                let tag = call.local_tag.clone();
                let bytes = build_response(&request, 200, "OK", Some(&tag), None, None)?;
                self.remember(&request, bytes.clone(), self.call_ref()?.peer);
                self.send_to_peer(&bytes)?;
            }
            Action::SendBye => self.send_bye()?,
            Action::SendByeSuccess => {
                let call = self.call_ref()?;
                let request = call
                    .incoming_bye
                    .clone()
                    .ok_or("no incoming BYE to answer")?;
                let tag = call.local_tag.clone();
                let bytes = build_response(&request, 200, "OK", Some(&tag), None, None)?;
                self.remember(&request, bytes.clone(), self.call_ref()?.peer);
                self.send_to_peer(&bytes)?;
            }
            Action::StartMedia => {
                let started = match self.call_ref()?.remote_rtp {
                    Some(remote) => self.media.start(remote),
                    None => Err("peer did not offer usable PCMU audio".into()),
                };
                match started {
                    Ok(()) => self.media_running = true,
                    Err(error) => {
                        self.report(format!("Cannot start audio: {error}"));
                        self.pending.push_back(Input::Command(Command::Hangup));
                    }
                }
            }
            Action::StopMedia => {
                if self.media_running {
                    let stats = self.media.stats();
                    self.emit(Event::MediaStats {
                        sent: stats.sent_packets,
                        received: stats.received_packets,
                    });
                }
                self.media.stop();
                self.media_running = false;
            }
            Action::ReportEstablished => self.emit(Event::Established),
            Action::ReportEnded => {
                // Also releases a reserved RTP port when media never started.
                self.media.stop();
                self.media_running = false;
                let reason = self
                    .call
                    .as_ref()
                    .and_then(|call| call.reason)
                    .unwrap_or(EndReason::Completed);
                self.emit(Event::Ended(reason));
            }
        }
        Ok(())
    }

    fn send_success_ack(&mut self) -> Result<()> {
        if let Some(bytes) = self.call_ref()?.ack_bytes.clone() {
            // Retransmitted 2xx: repeat the same ACK.
            let peer = self.call_ref()?.peer;
            return self.send(&bytes, peer);
        }

        let branch = self.branch();
        let local = self.local;
        let contact = self.contact();
        let call = self.call_mut()?;
        let response = call
            .last_invite_response
            .clone()
            .ok_or("no 2xx response to acknowledge")?;

        call.remote_rtp = media_address(&response.body).ok();
        let mut dialog = EstablishedDialog::new(DialogConfig {
            remote_target: contact_uri(&response.headers)?,
            local_from: tagged(header(&call.invite.headers, "from")?)?,
            remote_to: tagged(header(&response.headers, "to")?)?,
            call_id: call.call_id.clone(),
            local_sent_by: local.to_string(),
            local_contact: contact,
            invite_cseq: call.invite_headers.cseq,
            invite_branch: call.invite_headers.branch.clone(),
        })?;
        let bytes = dialog.build_success_ack(&branch)?.serialize()?;
        call.dialog = Some(dialog);
        call.ack_bytes = Some(bytes.clone());
        let peer = call.peer;
        self.send(&bytes, peer)
    }

    fn send_bye(&mut self) -> Result<()> {
        let branch = self.branch();
        let local = self.local;
        let contact = self.contact();
        let call = self.call_mut()?;

        if call.dialog.is_none() {
            // Called side: the dialog is built from the received INVITE.
            let invite = &call.invite.headers;
            call.dialog = Some(EstablishedDialog::new(DialogConfig {
                remote_target: contact_uri(invite)?,
                local_from: format!(
                    "<{}>;tag={}",
                    split_address(header(invite, "to")?).0,
                    call.local_tag
                ),
                remote_to: tagged(header(invite, "from")?)?,
                call_id: call.call_id.clone(),
                local_sent_by: local.to_string(),
                local_contact: contact,
                invite_cseq: 1,
                invite_branch: call.invite_headers.branch.clone(),
            })?);
        }
        let dialog = call.dialog.as_mut().ok_or("no dialog")?;
        let request = dialog.build_bye(&branch)?;
        let headers = TransactionHeaders::from_message(&Message::Request(request.clone()))?;
        let bytes = request.serialize()?;
        call.bye_request = Some(request);
        call.bye_headers = Some(headers);
        let (peer, id) = (call.peer, call.call_id.clone());
        self.send(&bytes, peer)?;
        self.start_retx(RetxKind::Bye, id, bytes, peer, self.timers.t2);
        Ok(())
    }

    // ----- sending and timers ---------------------------------------------

    fn send(&self, bytes: &[u8], peer: SocketAddr) -> Result<()> {
        self.socket.send_to(bytes, peer)?;
        Ok(())
    }

    fn send_to_peer(&self, bytes: &[u8]) -> Result<()> {
        self.send(bytes, self.call_ref()?.peer)
    }

    fn start_retx(
        &mut self,
        kind: RetxKind,
        call_id: String,
        bytes: Vec<u8>,
        peer: SocketAddr,
        cap: Duration,
    ) {
        let now = Instant::now();
        self.retx.push(Retx {
            kind,
            call_id,
            bytes,
            peer,
            interval: self.timers.t1,
            cap,
            next: now + self.timers.t1,
            deadline: now + self.timers.give_up,
        });
    }

    fn start_final_retx(&mut self, bytes: Vec<u8>) -> Result<()> {
        let call = self.call_ref()?;
        let (id, peer) = (call.call_id.clone(), call.peer);
        self.start_retx(RetxKind::Final, id, bytes, peer, self.timers.t2);
        Ok(())
    }

    fn stop_retx(&mut self, call_id: &str, kinds: &[RetxKind]) {
        self.retx
            .retain(|retx| !(retx.call_id == call_id && kinds.contains(&retx.kind)));
    }

    fn tick_timers(&mut self) {
        let now = Instant::now();
        let mut expired = Vec::new();
        for retx in &mut self.retx {
            if now >= retx.deadline {
                expired.push((retx.kind, retx.call_id.clone()));
            } else if now >= retx.next {
                // A lost retransmission is repaired by the next one.
                let _ = self.socket.send_to(&retx.bytes, retx.peer);
                retx.interval = (retx.interval * 2).min(retx.cap);
                retx.next = now + retx.interval;
            }
        }
        self.retx.retain(|retx| now < retx.deadline);
        self.retained.retain(|entry| now < entry.expires);

        for (kind, call_id) in expired {
            let current = self
                .call
                .as_ref()
                .is_some_and(|call| call.call_id == call_id);
            let fire = current
                && matches!(
                    (kind, self.engine.state()),
                    (RetxKind::Invite | RetxKind::Cancel, State::Outgoing)
                        | (RetxKind::Bye, State::Ending)
                        | (RetxKind::Final, State::Answered)
                );
            if fire {
                self.dispatch_reporting(Input::Timeout);
            }
        }
    }

    fn pump_media(&mut self) {
        if !self.media_running {
            return;
        }
        if let Err(error) = self.media.pump() {
            self.report(format!("Audio failed: {error}"));
            self.media_running = false;
            self.pending.push_back(Input::Command(Command::Hangup));
        }
    }

    fn remember(&mut self, request: &Request, bytes: Vec<u8>, peer: SocketAddr) {
        let Some(key) = retained_key(request) else {
            return;
        };
        let expires = Instant::now() + self.timers.give_up;
        if let Some(entry) = self.retained.iter_mut().find(|entry| entry.key == key) {
            entry.bytes = bytes;
            entry.peer = peer;
            entry.expires = expires;
        } else {
            self.retained.push(Retained {
                key,
                bytes,
                peer,
                expires,
            });
        }
    }

    /// Builds, remembers and returns a response to the current INVITE.
    fn invite_response(
        &mut self,
        status: u16,
        reason: &str,
        sdp: Option<String>,
    ) -> Result<Vec<u8>> {
        let contact = self.contact();
        let call = self.call_ref()?;
        let (invite, tag, peer) = (call.invite.clone(), call.local_tag.clone(), call.peer);
        let bytes = build_response(&invite, status, reason, Some(&tag), Some(&contact), sdp)?;
        self.remember(&invite, bytes.clone(), peer);
        Ok(bytes)
    }

    // ----- receiving -------------------------------------------------------

    fn receive_one(&mut self) -> io::Result<()> {
        match self.socket.recv_from(&mut self.buffer) {
            Ok((length, source)) => {
                let data = self.buffer[..length].to_vec();
                self.on_datagram(&data, source);
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        // Windows reports an ICMP "port unreachable" this way.
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn on_datagram(&mut self, data: &[u8], source: SocketAddr) {
        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        let Ok(message) = Message::parse(text) else {
            return;
        };
        let result = match &message {
            Message::Request(request) => self.on_request(&message, request, source),
            Message::Response(response) => self.on_response(&message, response, source),
        };
        if let Err(error) = result {
            self.report(error.to_string());
        }
    }

    fn on_request(
        &mut self,
        message: &Message,
        request: &Request,
        source: SocketAddr,
    ) -> Result<()> {
        let Ok(headers) = TransactionHeaders::from_message(message) else {
            return Ok(());
        };
        let method = request.method.as_str();

        if method != "ACK"
            && let Some(key) = retained_key(request)
            && let Some(entry) = self
                .retained
                .iter()
                .find(|entry| entry.key == key && entry.peer == source)
        {
            let bytes = entry.bytes.clone();
            return self.send(&bytes, source);
        }

        match method {
            "ACK" => self.on_ack(&headers, source),
            "INVITE" => self.on_invite(request, source),
            "BYE" => self.on_bye(request, &headers, source),
            "CANCEL" => self.on_cancel(request, &headers, source),
            _ => self.reply_stateless(request, source, 405, "Method Not Allowed"),
        }
    }

    fn is_current(&self, call_id: &str, source: SocketAddr) -> bool {
        self.call
            .as_ref()
            .is_some_and(|call| call.call_id == call_id && call.peer == source)
    }

    fn on_ack(&mut self, headers: &TransactionHeaders, source: SocketAddr) -> Result<()> {
        if !self.is_current(&headers.call_id, source) {
            return Ok(());
        }
        self.stop_retx(&headers.call_id, &[RetxKind::Final]);
        let invite_cseq = self.call_ref()?.invite_headers.cseq;
        if self.engine.state() == State::Answered && headers.cseq == invite_cseq {
            self.dispatch(Input::IncomingAck)?;
        }
        Ok(())
    }

    fn on_invite(&mut self, request: &Request, source: SocketAddr) -> Result<()> {
        if self.engine.state() != State::Idle {
            return self.reply_stateless(request, source, 486, "Busy Here");
        }
        let remote_rtp = match media_address(&request.body) {
            Ok(address) => address,
            Err(_) => {
                return self.reply_stateless(request, source, 488, "Not Acceptable Here");
            }
        };
        if contact_uri(&request.headers).is_err() {
            return self.reply_stateless(request, source, 400, "Bad Request");
        }
        let local_rtp = match self.media.prepare(self.local.ip()) {
            Ok(address) => address,
            Err(error) => {
                self.report(format!("Cannot reserve audio port: {error}"));
                return self.reply_stateless(request, source, 500, "Server Internal Error");
            }
        };

        let tag = format!("rs-{}", self.unique());
        let mut call = Call::new(source, request.clone(), local_rtp, tag)?;
        call.remote_rtp = Some(remote_rtp);
        self.call = Some(call);

        let trying = self.invite_response(100, "Trying", None)?;
        self.send(&trying, source)?;
        self.dispatch(Input::IncomingInvite)?;
        self.emit(Event::Incoming { peer: source });
        Ok(())
    }

    fn on_bye(
        &mut self,
        request: &Request,
        headers: &TransactionHeaders,
        source: SocketAddr,
    ) -> Result<()> {
        let live = matches!(
            self.engine.state(),
            State::Answered | State::Established | State::Ending
        );
        if !(live && self.is_current(&headers.call_id, source)) {
            return self.reply_stateless(request, source, 481, "Call/Transaction Does Not Exist");
        }
        self.stop_retx(&headers.call_id, &[RetxKind::Final]);
        self.call_mut()?.incoming_bye = Some(request.clone());
        self.dispatch(Input::IncomingBye)
    }

    fn on_cancel(
        &mut self,
        request: &Request,
        headers: &TransactionHeaders,
        source: SocketAddr,
    ) -> Result<()> {
        let matches_invite = self.is_current(&headers.call_id, source)
            && self
                .call
                .as_ref()
                .is_some_and(|call| call.invite_headers.branch == headers.branch)
            && matches!(self.engine.state(), State::Incoming | State::Answered);
        if !matches_invite {
            return self.reply_stateless(request, source, 481, "Call/Transaction Does Not Exist");
        }
        self.call_mut()?.incoming_cancel = Some(request.clone());
        self.dispatch(Input::IncomingCancel)
    }

    fn reply_stateless(
        &self,
        request: &Request,
        source: SocketAddr,
        status: u16,
        reason: &str,
    ) -> Result<()> {
        let tag = format!("rs-stateless-{}", self.local.port());
        let bytes = build_response(request, status, reason, Some(&tag), None, None)?;
        self.send(&bytes, source)
    }

    fn on_response(
        &mut self,
        message: &Message,
        response: &Response,
        source: SocketAddr,
    ) -> Result<()> {
        let Ok(headers) = TransactionHeaders::from_message(message) else {
            return Ok(());
        };
        let Some(call) = self.call.as_ref() else {
            return Ok(());
        };
        if call.peer != source || call.call_id != headers.call_id {
            return Ok(());
        }
        let status = response.status_code;

        match headers.method.as_str() {
            "INVITE" if call.invite_headers.matches_response(&headers) => {
                self.on_invite_response(response, status)
            }
            "CANCEL"
                if call
                    .cancel_headers
                    .as_ref()
                    .is_some_and(|sent| sent.matches_response(&headers)) =>
            {
                if status >= 200 {
                    // Keep waiting for the INVITE's own final response, but do
                    // not retransmit the CANCEL any more.
                    for retx in &mut self.retx {
                        if retx.kind == RetxKind::Cancel && retx.call_id == headers.call_id {
                            retx.next = retx.deadline;
                        }
                    }
                }
                Ok(())
            }
            "BYE"
                if call
                    .bye_headers
                    .as_ref()
                    .is_some_and(|sent| sent.matches_response(&headers)) =>
            {
                if status < 200 {
                    return Ok(());
                }
                self.stop_retx(&headers.call_id, &[RetxKind::Bye]);
                if (200..300).contains(&status)
                    && let Some(dialog) = self.call_mut()?.dialog.as_mut()
                {
                    // A refused state change here only means the BYE was not sent by us.
                    let _ = dialog.finish_local_bye();
                }
                if self.engine.state() == State::Ending {
                    self.dispatch(Input::ByeResponse(status))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn on_invite_response(&mut self, response: &Response, status: u16) -> Result<()> {
        let id = self.call_ref()?.call_id.clone();
        let state = self.engine.state();

        if status < 200 {
            self.stop_retx(&id, &[RetxKind::Invite]);
            if state != State::Outgoing {
                return Ok(());
            }
            self.dispatch(Input::InviteResponse(status))?;
            let call = self.call_mut()?;
            if (180..=183).contains(&status) && !call.ringing_reported {
                call.ringing_reported = true;
                self.emit(Event::Ringing);
            }
            return Ok(());
        }

        self.stop_retx(&id, &[RetxKind::Invite, RetxKind::Cancel]);
        match state {
            State::Outgoing => {
                self.call_mut()?.last_invite_response = Some(response.clone());
                self.dispatch(Input::InviteResponse(status))
            }
            State::Established | State::Ending if (200..300).contains(&status) => {
                self.dispatch(Input::InviteResponse(status))
            }
            State::Idle if status >= 300 => {
                // Our ACK was lost and the peer repeats its final response.
                if let Some(bytes) = self.call_ref()?.failure_ack.clone() {
                    let peer = self.call_ref()?.peer;
                    self.send(&bytes, peer)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // ----- outgoing call ---------------------------------------------------

    fn dial(&mut self, peer: SocketAddr) -> Result<()> {
        if self.engine.state() != State::Idle {
            return Err("a call is already in progress".into());
        }
        if peer.port() == 0 || peer.ip().is_unspecified() || peer.is_ipv4() != self.local.is_ipv4()
        {
            return Err("invalid destination address".into());
        }

        let local_rtp = self.media.prepare(self.local.ip())?;
        let user = self.user.clone();
        let local = self.local;
        let call_id = format!("{}@{}", self.unique(), local.ip());
        let branch = self.branch();
        let tag = self.unique();
        let invite = InviteRequest::new(
            format!("sip:{user}@{peer}"),
            format!("sip:{user}@{local}"),
            format!("sip:{user}@{peer}"),
            format!("sip:{user}@{local}"),
            local.to_string(),
            call_id,
            branch,
            tag,
        )
        .body(make_sdp(local_rtp).into_bytes())
        .build()?;

        let local_tag = format!("rs-{}", self.unique());
        self.call = Some(Call::new(peer, invite, local_rtp, local_tag)?);
        self.dispatch(Input::Command(Command::Dial))
    }
}

// ----- message helpers -----------------------------------------------------

fn reason_phrase(status: u16) -> &'static str {
    match status {
        180 => "Ringing",
        200 => "OK",
        486 => "Busy Here",
        487 => "Request Terminated",
        _ => "Declined",
    }
}

fn header<'a>(headers: &'a Headers, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .ok_or_else(|| format!("missing {name} header").into())
}

fn retained_key(request: &Request) -> Option<String> {
    let call_id = request.headers.get("call-id")?;
    let cseq = request.headers.get("cseq")?;
    Some(format!("{call_id}|{cseq}"))
}

fn has_tag(value: &str) -> bool {
    value
        .split(';')
        .skip(1)
        .any(|part| part.trim().to_ascii_lowercase().starts_with("tag="))
}

fn build_response(
    request: &Request,
    status: u16,
    reason: &str,
    to_tag: Option<&str>,
    contact: Option<&str>,
    sdp: Option<String>,
) -> Result<Vec<u8>> {
    let to = header(&request.headers, "to")?;
    let to = match to_tag {
        Some(tag) if !has_tag(to) => format!("{to};tag={tag}"),
        _ => to.to_owned(),
    };
    let mut builder = ResponseBuilder::new(status, reason);
    for via in request.headers.get_all("via") {
        builder = builder.append_header("via", via)?;
    }
    builder = builder
        .header("from", header(&request.headers, "from")?)?
        .header("to", to)?
        .header("call-id", header(&request.headers, "call-id")?)?
        .header("cseq", header(&request.headers, "cseq")?)?;
    if let Some(contact) = contact {
        builder = builder.header("contact", contact)?;
    }
    if let Some(sdp) = sdp {
        builder = builder
            .header("content-type", "application/sdp")?
            .body(sdp.into_bytes());
    }
    Ok(builder.build()?.serialize()?)
}

/// Splits a From/To/Contact value into its bare URI and tag parameter.
fn split_address(value: &str) -> (String, Option<String>) {
    let value = value.trim();
    let (uri, parameters) = match (value.find('<'), value.find('>')) {
        (Some(start), Some(end)) if start < end => (&value[start + 1..end], &value[end + 1..]),
        _ => match value.split_once(';') {
            Some((uri, rest)) => (uri, rest),
            None => (value, ""),
        },
    };
    let tag = parameters
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("tag"))
        .map(|(_, tag)| tag.trim().to_owned());
    (uri.trim().to_owned(), tag)
}

/// Rewrites an address as `<uri>;tag=value`, the form the dialog requires.
fn tagged(value: &str) -> Result<String> {
    let (uri, tag) = split_address(value);
    let tag = tag.ok_or("address has no tag")?;
    Ok(format!("<{uri}>;tag={tag}"))
}

fn contact_uri(headers: &Headers) -> Result<String> {
    let (uri, _) = split_address(header(headers, "contact")?);
    if !uri.starts_with("sip:") || uri.contains(',') || uri.contains(char::is_whitespace) {
        return Err("unsupported Contact".into());
    }
    Ok(uri)
}

fn make_sdp(address: SocketAddr) -> String {
    let family = if address.is_ipv4() { "IP4" } else { "IP6" };
    let ip = address.ip();
    format!(
        "v=0\r\no=phone 1 1 IN {family} {ip}\r\ns=Rust direct call\r\n\
         c=IN {family} {ip}\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\n\
         a=rtpmap:0 PCMU/8000\r\na=sendrecv\r\n",
        address.port()
    )
}

/// Audio address from an SDP body; requires RTP/AVP with PCMU (payload 0).
fn media_address(body: &[u8]) -> Result<SocketAddr> {
    let sdp = Sdp::parse(std::str::from_utf8(body)?)?;
    let audio = sdp.media(&MediaKind::Audio).ok_or("no audio in SDP")?;
    if audio.port == 0
        || audio.protocol != "RTP/AVP"
        || !audio.formats.iter().any(|format| format == "0")
    {
        return Err("audio must be enabled RTP/AVP with PCMU payload 0".into());
    }
    let connection = audio
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .ok_or("no media connection address")?;
    let ip: IpAddr = connection.address.parse()?;
    if ip.is_unspecified() {
        return Err("hold or unspecified media address is not supported".into());
    }
    Ok(SocketAddr::new(ip, audio.port))
}
