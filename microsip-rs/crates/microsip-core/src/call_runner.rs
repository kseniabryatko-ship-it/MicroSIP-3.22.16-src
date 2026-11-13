//! Network executor for [`CallEngine`] over direct UDP.
//!
//! One thread owns the SIP socket, the engine and the media backend. The user
//! interface talks to it through [`CallHandle`]: commands go in, [`Event`]s
//! come out. Incoming datagrams are classified here, before the engine, so a
//! retransmitted INVITE or a stray request never becomes an engine input.
//!
//! Scope: UDP, one call. One account can be registered on a SIP server through
//! the same socket; the REGISTER logic lives in `registration`. While it is
//! registered, a number can be called through that server: the INVITE answers
//! one 401/407 challenge, and the Record-Route set of a loose-routing proxy is
//! used for ACK and BYE (every request still goes to the first hop).
//!
//! NAT: requests ask for `rport`, the Contact follows the public address the
//! registrar reported, and a keep-alive goes to the registrar while the
//! account is registered. The SDP still carries the local address, so audio
//! behind NAT depends on the server answering to where our RTP comes from. Retransmission timers follow
//! the RFC 3261 doubling scheme with fixed constants; the older `transaction`
//! module is not used here.

use crate::call_engine::{Action, CallEngine, Command, Input, State};
use crate::call_media::{Media, MediaQuality};
use crate::codec::Codec;
use crate::dialog::{DialogConfig, EstablishedDialog};
use crate::invite_control::{build_cancel, build_failure_ack};
use crate::registration::{Account, Output, Registration, RegistrationState, add_rport};
use crate::sip::message::{Headers, InviteRequest, Message, Request, Response, ResponseBuilder};
use crate::sip::sdp::{MediaKind, Sdp};
use crate::tones::Tone;
use crate::transaction_headers::TransactionHeaders;
use crate::transport::resolve_socket_address;
use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type BoxError = Box<dyn Error + Send + Sync>;
type Result<T> = std::result::Result<T, BoxError>;

const TICK: Duration = Duration::from_millis(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Double CRLF, the keep-alive of RFC 5626; SIP servers ignore or echo it.
const KEEPALIVE: &[u8] = b"\r\n\r\n";

/// Commands from the user interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Dial(SocketAddr),
    /// Calls `IP:port` directly, or a number (`100`, `100@domain`) through
    /// the SIP server the account is registered on.
    DialTarget(String),
    Answer,
    Reject,
    Hangup,
    /// Sends a keypad digit (`0`-`9`, `*`, `#`, `A`-`D`) during a call.
    Dtmf(char),
    /// Stops (`true`) or resumes sending the microphone during a call.
    Mute(bool),
    /// Puts the connected call on hold (`true`) or resumes it. The result
    /// is reported with [`Event::Hold`].
    Hold(bool),
    /// Hands the connected call over to another number (blind transfer).
    /// Progress is reported with [`Event::Transfer`]; when the transfer
    /// succeeds this phone's part of the call ends.
    Transfer(String),
    /// While on, incoming calls are refused as busy without ringing and
    /// reported with [`Event::Declined`].
    DoNotDisturb(bool),
    /// Makes this line (0, 1, ...) the one the commands go to. Selecting a
    /// line does not change any call; use `Hold` to switch the conversation.
    SelectLine(usize),
    /// Joins all connected lines into one conversation (`true`), or goes
    /// back to talking on the selected line only.
    Conference(bool),
    /// Connects the party of the selected line with the party of the given
    /// line and leaves both calls (attended transfer). Reported with
    /// [`Event::Transfer`].
    TransferToLine(usize),
    /// Sends all incoming calls to this number instead of ringing;
    /// `None` switches forwarding off.
    Forward(Option<String>),
    /// Records the connected call to this WAV file, or stops recording with
    /// `None`. Reported with [`Event::Recording`]; the recording also stops
    /// when the call ends.
    Record(Option<PathBuf>),
    /// Registers the account on its SIP server and keeps it registered.
    /// Progress is reported with [`Event::Registration`].
    Register(Account),
    /// Removes the registration made by [`UiCommand::Register`].
    Unregister,
    /// Turns [`Event::Trace`] on or off. It is off at the start.
    Trace(bool),
    Shutdown,
}

/// Progress of a call transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    /// The server took the request and is calling the new number.
    Accepted,
    /// The other side is connected to the new number.
    Completed,
    /// The transfer did not happen (SIP status); the call goes on.
    Failed(u16),
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
    /// Who is calling: sent just before [`Event::Incoming`]. `number` is the
    /// user part of the caller's address, `name` the display name (may be
    /// empty).
    Caller {
        number: String,
        name: String,
    },
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
    /// How well the received audio played, sent just before the media stops
    /// when real audio was used.
    AudioQuality(MediaQuality),
    /// The call is now on hold by this phone (`true`) or resumed.
    Hold(bool),
    /// The other side put the call on hold (`true`) or resumed it.
    RemoteHold(bool),
    /// The call is being recorded to this file, or recording stopped.
    Recording(Option<PathBuf>),
    /// Progress of the transfer asked for with [`UiCommand::Transfer`].
    Transfer(TransferState),
    /// The events that follow are about this line. Only sent by a phone
    /// with more than one line; until the first one the line is 0.
    Line(usize),
    /// The commands now go to this line.
    Selected(usize),
    /// All connected lines were joined into a conference, or separated.
    Conference(bool),
    /// A call was sent on to another number because forwarding is on.
    Forwarded {
        number: String,
        name: String,
        to: String,
    },
    /// A call was refused because do-not-disturb is on.
    Declined {
        number: String,
        name: String,
    },
    Ended(EndReason),
    /// The account's registration changed state.
    Registration(RegistrationState),
    /// A SIP message as sent or received, for diagnostics. The Digest
    /// `response` value is replaced, so the text holds no credentials.
    Trace {
        outgoing: bool,
        peer: SocketAddr,
        text: String,
    },
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
    spawn_runner(local, user, timers, move || vec![make_media()])
}

/// Starts an executor with several lines, so that a second call can be
/// received or made while the first one is held. `make_media` is called once
/// per line, on the new thread.
pub fn spawn_lines<M, F>(
    local: SocketAddr,
    user: &str,
    timers: Timers,
    lines: usize,
    make_media: F,
) -> io::Result<CallHandle>
where
    M: Media + 'static,
    F: Fn() -> M + Send + 'static,
{
    spawn_runner(local, user, timers, move || {
        (0..lines.max(1)).map(|_| make_media()).collect()
    })
}

fn spawn_runner<M, F>(
    local: SocketAddr,
    user: &str,
    timers: Timers,
    make_medias: F,
) -> io::Result<CallHandle>
where
    M: Media + 'static,
    F: FnOnce() -> Vec<M> + Send + 'static,
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
        let runner = Runner::new(socket, local, user, timers, make_medias(), event_sender);
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
    Register,
    ReInvite,
    Refer,
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
    /// Telephone-event payload type the other side offered or accepted.
    dtmf_payload_type: Option<u8>,
    /// Voice codec agreed with the other side.
    codec: Codec,
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
    /// Set for a call placed through the registrar.
    server: Option<ServerCall>,
    /// Route header values for requests inside the dialog.
    route_set: Vec<String>,
    /// The INVITE replaced by an authorized one, with the ACK sent for it.
    superseded: Option<(TransactionHeaders, Vec<u8>)>,
    /// Version of our session description; raised whenever it changes.
    sdp_version: u32,
    /// Our hold or resume request that is waiting for its answer.
    reinvite: Option<ReInvite>,
    /// The last answered re-INVITE with its ACK, for a repeated answer.
    reinvite_ack: Option<(TransactionHeaders, Vec<u8>)>,
    local_hold: bool,
    remote_hold: bool,
    /// Our transfer request that is waiting for its answer.
    refer: Option<TransactionHeaders>,
    /// The line whose call this one is being joined with by an attended
    /// transfer; it is ended too when the transfer is done.
    transfer_partner: Option<usize>,
    /// The BYE was already repeated with credentials once.
    bye_authorized: bool,
}

struct ReInvite {
    request: Request,
    headers: TransactionHeaders,
    hold: bool,
}

struct ServerCall {
    target: String,
    from_uri: String,
    contact_uri: String,
    from_tag: String,
    authorized: bool,
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
            dtmf_payload_type: None,
            codec: Codec::Pcmu,
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
            server: None,
            route_set: Vec::new(),
            superseded: None,
            sdp_version: 1,
            reinvite: None,
            reinvite_ack: None,
            local_hold: false,
            remote_hold: false,
            refer: None,
            transfer_partner: None,
            bye_authorized: false,
        })
    }
}

/// One line of the phone: it can hold one call, with its own audio.
struct Line<M: Media> {
    engine: CallEngine,
    call: Option<Call>,
    media: M,
    media_running: bool,
    pending: VecDeque<Input>,
}

impl<M: Media> Line<M> {
    fn new(media: M) -> Self {
        Self {
            engine: CallEngine::new(),
            call: None,
            media,
            media_running: false,
            pending: VecDeque::new(),
        }
    }
}

struct Runner<M: Media> {
    socket: UdpSocket,
    local: SocketAddr,
    user: String,
    timers: Timers,
    /// The phone's lines. Most of the code below works on `lines[cur]`.
    lines: Vec<Line<M>>,
    /// The line the code is working on right now.
    cur: usize,
    /// The line the user's commands go to.
    selected: usize,
    /// The line the last event was about, as told to the interface.
    told_line: std::cell::Cell<usize>,
    /// Every connected line talks at once; nothing is held automatically.
    conference: bool,
    /// Where all incoming calls are sent instead of ringing here.
    forward_to: Option<String>,
    events: Sender<Event>,
    registration: Option<Registration>,
    trace: bool,
    do_not_disturb: bool,
    retx: Vec<Retx>,
    retained: Vec<Retained>,
    counter: u64,
    buffer: Vec<u8>,
}

impl<M: Media> Runner<M> {
    fn new(
        socket: UdpSocket,
        local: SocketAddr,
        user: String,
        timers: Timers,
        medias: Vec<M>,
        events: Sender<Event>,
    ) -> Self {
        Self {
            socket,
            local,
            user,
            timers,
            lines: medias.into_iter().map(Line::new).collect(),
            cur: 0,
            selected: 0,
            told_line: std::cell::Cell::new(0),
            conference: false,
            forward_to: None,
            events,
            registration: None,
            trace: false,
            do_not_disturb: false,
            retx: Vec::new(),
            retained: Vec::new(),
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
            for line in 0..self.lines.len() {
                self.cur = line;
                self.pump_media();
            }
            self.tick_timers();
            for line in 0..self.lines.len() {
                self.cur = line;
                self.drain_pending();
            }
            // Whatever is not about a particular call belongs to the
            // selected line.
            self.cur = self.selected;

            let idle = self
                .lines
                .iter()
                .all(|line| line.engine.state() == State::Idle);
            let settled = idle && self.registration.is_none();
            if let Some(deadline) = shutdown
                && (settled || Instant::now() >= deadline)
            {
                break;
            }
        }
        for line in &mut self.lines {
            line.media.stop();
        }
    }

    fn begin_shutdown(&mut self) {
        self.unregister();
        for line in 0..self.lines.len() {
            self.cur = line;
            let input = match self.lines[self.cur].engine.state() {
                State::Outgoing | State::Established => Input::Command(Command::Hangup),
                State::Incoming => Input::Command(Command::Reject),
                State::Idle | State::Answered | State::Ending => continue,
            };
            self.dispatch_reporting(input);
        }
    }

    fn on_command(&mut self, command: UiCommand) {
        self.cur = self.selected;
        // A new call needs a line that is free.
        if matches!(command, UiCommand::Dial(_) | UiCommand::DialTarget(_))
            && self.lines[self.cur].engine.state() != State::Idle
            && let Some(line) = self.free_line()
        {
            self.select(line);
        }
        // "Answer" means the ringing call, wherever it is.
        if matches!(command, UiCommand::Answer | UiCommand::Reject)
            && self.lines[self.cur].engine.state() != State::Incoming
            && let Some(line) = (0..self.lines.len())
                .find(|line| self.lines[*line].engine.state() == State::Incoming)
        {
            self.select(line);
        }
        let result = match command {
            UiCommand::Dial(peer) => {
                let was_idle = self.lines[self.cur].engine.state() == State::Idle;
                let result = self.dial(peer);
                if result.is_err() && was_idle && self.lines[self.cur].engine.state() == State::Idle
                {
                    // Tell the interface the attempt is over, not just why.
                    if let Err(error) = &result {
                        self.report(error.to_string());
                    }
                    self.emit(Event::Ended(EndReason::Failed));
                    return;
                }
                result
            }
            UiCommand::DialTarget(target) => {
                let was_idle = self.lines[self.cur].engine.state() == State::Idle;
                let result = self.dial_target(&target);
                if result.is_err() && was_idle && self.lines[self.cur].engine.state() == State::Idle
                {
                    if let Err(error) = &result {
                        self.report(error.to_string());
                    }
                    self.emit(Event::Ended(EndReason::Failed));
                    return;
                }
                result
            }
            UiCommand::Answer => {
                let answered = self.dispatch(Input::Command(Command::Answer));
                if answered.is_ok() {
                    // The call just answered is the one to talk on.
                    self.hold_other_lines();
                }
                answered
            }
            UiCommand::Reject => self.dispatch(Input::Command(Command::Reject)),
            // The other side may end the call at the very moment the user
            // presses the button; there is then nothing left to hang up.
            UiCommand::Hangup if self.lines[self.cur].engine.state() == State::Idle => Ok(()),
            UiCommand::Hangup => self.dispatch(Input::Command(Command::Hangup)),
            UiCommand::Dtmf(digit) => {
                if self.lines[self.cur].media_running {
                    self.lines[self.cur].media.send_dtmf(digit)
                } else {
                    Err("digits can be sent only during a connected call".into())
                }
            }
            UiCommand::Mute(muted) => {
                self.lines[self.cur].media.set_muted(muted);
                Ok(())
            }
            UiCommand::Hold(hold) => {
                let done = self.hold(hold);
                if done.is_ok() && !hold {
                    // Resuming one call puts the other on hold.
                    self.hold_other_lines();
                }
                done
            }
            UiCommand::SelectLine(line) => {
                if line < self.lines.len() {
                    self.select(line);
                    Ok(())
                } else {
                    Err(format!("this phone has no line {}", line + 1).into())
                }
            }
            UiCommand::Conference(on) => self.set_conference(on),
            UiCommand::TransferToLine(line) => self.transfer_to_line(line),
            UiCommand::Forward(target) => {
                self.forward_to = target
                    .map(|target| target.trim().to_owned())
                    .filter(|target| !target.is_empty());
                Ok(())
            }
            UiCommand::Transfer(target) => self.transfer(&target),
            UiCommand::DoNotDisturb(on) => {
                self.do_not_disturb = on;
                Ok(())
            }
            UiCommand::Record(path) => {
                if path.is_some() && !self.lines[self.cur].media_running {
                    Err("only a connected call can be recorded".into())
                } else {
                    let result = self.lines[self.cur].media.record(path.as_deref());
                    if result.is_ok() {
                        self.emit(Event::Recording(path));
                    }
                    result
                }
            }
            UiCommand::Register(account) => {
                self.register(account);
                Ok(())
            }
            UiCommand::Unregister => {
                if self.registration.is_none() {
                    self.emit(Event::Registration(RegistrationState::Unregistered));
                }
                self.unregister();
                Ok(())
            }
            UiCommand::Trace(enabled) => {
                self.trace = enabled;
                Ok(())
            }
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

    /// Starts or stops the phone's own ringing. A phone that cannot ring
    /// (no speaker) still works, so a failure is only reported.
    fn ring(&mut self, tone: Option<Tone>) {
        if let Err(error) = self.lines[self.cur].media.ring(tone) {
            self.report(format!("Cannot play the ringing sound: {error}"));
        }
    }

    fn drain_pending(&mut self) {
        while let Some(input) = self.lines[self.cur].pending.pop_front() {
            self.dispatch_reporting(input);
        }
    }

    fn report(&self, message: String) {
        self.emit(Event::Error(message));
    }

    fn emit(&self, event: Event) {
        // With several lines the interface is told which one the following
        // events are about, whenever that changes.
        if self.lines.len() > 1 && self.told_line.get() != self.cur {
            self.told_line.set(self.cur);
            let _ = self.events.send(Event::Line(self.cur));
        }
        // The interface may have gone away; the executor then just finishes.
        let _ = self.events.send(event);
    }

    /// Makes `cur` the line that has this call, if any does.
    fn find_line(&mut self, call_id: &str, source: SocketAddr) -> bool {
        let found = self.lines.iter().position(|line| {
            line.call
                .as_ref()
                .is_some_and(|call| call.call_id == call_id && call.peer == source)
        });
        if let Some(line) = found {
            self.cur = line;
        }
        found.is_some()
    }

    /// A line without a call, preferring the selected one.
    fn free_line(&self) -> Option<usize> {
        let idle = |line: usize| self.lines[line].engine.state() == State::Idle;
        if idle(self.selected) {
            Some(self.selected)
        } else {
            (0..self.lines.len()).find(|line| idle(*line))
        }
    }

    /// Tells the interface which line its commands now go to.
    fn select(&mut self, line: usize) {
        if line < self.lines.len() && self.selected != line {
            self.selected = line;
            let _ = self.events.send(Event::Selected(line));
        }
        self.cur = self.selected;
    }

    /// Only one call talks at a time: every other connected line is put on
    /// hold, unless the lines are joined in a conference.
    fn hold_other_lines(&mut self) {
        if self.conference {
            return;
        }
        let keep = self.cur;
        for line in 0..self.lines.len() {
            let talking = self.lines[line].engine.state() == State::Established
                && self.lines[line]
                    .call
                    .as_ref()
                    .is_some_and(|call| !call.local_hold && call.reinvite.is_none());
            if line != keep && talking {
                self.cur = line;
                if let Err(error) = self.hold(true) {
                    self.report(error.to_string());
                }
            }
        }
        self.cur = keep;
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
        // While registered, calls use the account's user name and the
        // address the server reaches this phone at.
        match self.registration.as_ref() {
            Some(registration) => format!(
                "<sip:{}@{}>",
                registration.username(),
                registration.contact_address()
            ),
            None => format!("<sip:{}@{}>", self.user, self.local),
        }
    }

    // ----- registration ----------------------------------------------------

    fn register(&mut self, account: Account) {
        if self
            .registration
            .as_ref()
            .is_some_and(Registration::is_bound)
        {
            self.report("already registered; unregister first".to_owned());
            return;
        }
        // An attempt that never succeeded is simply replaced.
        if let Some(old) = self.registration.take() {
            let id = old.call_id().to_owned();
            self.stop_retx(&id, &[RetxKind::Register]);
        }

        let seed = self.unique();
        let created = account
            .server_host_port()
            .and_then(|(host, port)| {
                // Name resolution blocks this thread; it happens once per command.
                resolve_socket_address(&host, port)
                    .map_err(|error| format!("cannot resolve {host}: {error}"))
            })
            .and_then(|server| {
                // A loopback socket cannot send to another machine at all.
                if self.local.ip().is_loopback() && !server.ip().is_loopback() {
                    return Err(format!(
                        "the phone listens on {}, which cannot reach {server}; \
                         start it on this computer's network address",
                        self.local.ip()
                    ));
                }
                Registration::new(account, server, self.local, &seed)
            });
        match created {
            Ok(mut registration) => {
                let outputs = registration.start();
                self.registration = Some(registration);
                self.apply_registration(outputs);
            }
            Err(reason) => self.emit(Event::Registration(RegistrationState::Failed(reason))),
        }
    }

    fn unregister(&mut self) {
        if let Some(registration) = self.registration.as_mut() {
            let outputs = registration.unregister();
            self.apply_registration(outputs);
        }
    }

    fn apply_registration(&mut self, outputs: Vec<Output>) {
        let Some((server, id)) = self
            .registration
            .as_ref()
            .map(|registration| (registration.server(), registration.call_id().to_owned()))
        else {
            return;
        };
        for output in outputs {
            match output {
                Output::Send(bytes) => {
                    self.stop_retx(&id, &[RetxKind::Register]);
                    if let Err(error) = self.send(&bytes, server) {
                        // The retransmission below repeats the attempt.
                        self.report(format!("Cannot send REGISTER: {error}"));
                    }
                    self.start_retx(
                        RetxKind::Register,
                        id.clone(),
                        bytes,
                        server,
                        self.timers.t2,
                    );
                }
                Output::State(state) => self.emit(Event::Registration(state)),
            }
        }
        if self
            .registration
            .as_ref()
            .is_some_and(Registration::is_finished)
        {
            self.stop_retx(&id, &[RetxKind::Register]);
            self.registration = None;
        }
    }

    /// Returns true when the response belonged to the registration.
    fn on_register_response(
        &mut self,
        response: &Response,
        headers: &TransactionHeaders,
        source: SocketAddr,
    ) -> bool {
        let Some(registration) = self.registration.as_mut() else {
            return false;
        };
        if headers.method != "REGISTER"
            || source != registration.server()
            || headers.call_id != registration.call_id()
        {
            return false;
        }
        if let Some(outputs) = registration.on_response(response, headers, Instant::now()) {
            if response.status_code >= 200 {
                let id = headers.call_id.clone();
                self.stop_retx(&id, &[RetxKind::Register]);
            }
            self.apply_registration(outputs);
        }
        true
    }

    // ----- engine plumbing -------------------------------------------------

    fn dispatch(&mut self, input: Input) -> Result<()> {
        self.note_reason(input);
        let actions = self.lines[self.cur].engine.handle(input)?;
        for action in actions {
            if let Err(error) = self.execute(action) {
                self.report(format!("{action:?} failed: {error}"));
                match action {
                    // Without a sent request nothing will ever time out.
                    Action::SendBye | Action::SendInvite => {
                        self.lines[self.cur].pending.push_back(Input::Timeout);
                    }
                    Action::SendSuccessAck => {
                        self.lines[self.cur]
                            .pending
                            .push_back(Input::Command(Command::Hangup));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn note_reason(&mut self, input: Input) {
        let reason = match (self.lines[self.cur].engine.state(), input) {
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
        if let (Some(reason), Some(call)) = (reason, self.lines[self.cur].call.as_mut())
            && call.reason.is_none()
        {
            call.reason = Some(reason);
        }
    }

    fn call_ref(&self) -> Result<&Call> {
        self.lines[self.cur]
            .call
            .as_ref()
            .ok_or_else(|| "no current call".into())
    }

    fn call_mut(&mut self) -> Result<&mut Call> {
        self.lines[self.cur]
            .call
            .as_mut()
            .ok_or_else(|| "no current call".into())
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
                self.ring(None);
                // Answer with the telephone-event type the caller offered.
                let call = self.call_ref()?;
                // The answer names the one codec chosen from the offer.
                let sdp = make_sdp(
                    call.local_rtp,
                    &[call.codec],
                    call.dtmf_payload_type,
                    1,
                    "sendrecv",
                );
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
                let call = self.call_ref()?;
                let (remote_rtp, dtmf) = (call.remote_rtp, call.dtmf_payload_type);
                let codec = call.codec;
                let started = match remote_rtp {
                    Some(remote) => {
                        self.lines[self.cur].media.set_codec(codec);
                        self.lines[self.cur].media.set_dtmf_payload_type(dtmf);
                        self.lines[self.cur].media.start(remote)
                    }
                    None => Err("the other side offered no usable G.711 audio".into()),
                };
                match started {
                    Ok(()) => self.lines[self.cur].media_running = true,
                    Err(error) => {
                        self.report(format!("Cannot start audio: {error}"));
                        self.lines[self.cur]
                            .pending
                            .push_back(Input::Command(Command::Hangup));
                    }
                }
            }
            Action::StopMedia => {
                if self.lines[self.cur].media_running {
                    let stats = self.lines[self.cur].media.stats();
                    self.emit(Event::MediaStats {
                        sent: stats.sent_packets,
                        received: stats.received_packets,
                    });
                    if let Some(quality) = self.lines[self.cur].media.quality() {
                        self.emit(Event::AudioQuality(quality));
                    }
                }
                self.lines[self.cur].media.stop();
                self.lines[self.cur].media_running = false;
            }
            Action::ReportEstablished => self.emit(Event::Established),
            Action::ReportEnded => {
                // Also releases a reserved RTP port when media never started.
                self.lines[self.cur].media.stop();
                self.lines[self.cur].media_running = false;
                let reason = self.lines[self.cur]
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

        // In an answer the first codec listed is the one the other side uses.
        match negotiate(&response.body, false) {
            Ok((address, codec)) => {
                call.remote_rtp = Some(address);
                call.codec = codec;
            }
            Err(_) => call.remote_rtp = None,
        }
        call.dtmf_payload_type = telephone_event_payload_type(&response.body);
        // The caller uses the recorded route in reverse order.
        call.route_set = record_routes(&response.headers);
        call.route_set.reverse();
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
        let mut ack = dialog.build_success_ack(&branch)?;
        add_rport(&mut ack.headers)?;
        add_routes(&mut ack, &call.route_set)?;
        let bytes = ack.serialize()?;
        call.dialog = Some(dialog);
        call.ack_bytes = Some(bytes.clone());
        let peer = call.peer;
        self.send(&bytes, peer)
    }

    /// The called side builds its dialog from the received INVITE the first
    /// time it has to send a request of its own.
    fn ensure_dialog(&mut self) -> Result<()> {
        let local = self.local;
        let contact = self.contact();
        let call = self.call_mut()?;
        if call.dialog.is_none() {
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
        Ok(())
    }

    // ----- hold --------------------------------------------------------------

    /// Asks the other side to hold or resume the call with a re-INVITE.
    fn hold(&mut self, hold: bool) -> Result<()> {
        if self.lines[self.cur].engine.state() != State::Established {
            return Err("only a connected call can be put on hold".into());
        }
        self.ensure_dialog()?;
        let branch = self.branch();
        let call = self.call_mut()?;
        if call.reinvite.is_some() {
            return Err("the previous hold request is still waiting for an answer".into());
        }
        if call.local_hold == hold {
            return Ok(());
        }
        call.sdp_version += 1;
        // While we hold we still send (silence), but do not want to receive.
        let direction = if hold { "sendonly" } else { "sendrecv" };
        let sdp = make_sdp(
            call.local_rtp,
            &[call.codec],
            call.dtmf_payload_type,
            call.sdp_version,
            direction,
        );
        let dialog = call.dialog.as_mut().ok_or("no dialog")?;
        let mut request = dialog.build_reinvite(&branch)?;
        add_routes(&mut request, &call.route_set)?;
        request.headers.insert("content-type", "application/sdp")?;
        request.headers.remove("content-length");
        request
            .headers
            .insert("content-length", sdp.len().to_string())?;
        request.body = sdp.into_bytes();

        let headers = TransactionHeaders::from_message(&Message::Request(request.clone()))?;
        let bytes = request.serialize()?;
        let (peer, id) = (call.peer, call.call_id.clone());
        call.reinvite = Some(ReInvite {
            request,
            headers,
            hold,
        });
        self.send(&bytes, peer)?;
        self.start_retx(RetxKind::ReInvite, id, bytes, peer, self.timers.t2);
        Ok(())
    }

    fn on_reinvite_response(&mut self, response: &Response, status: u16) -> Result<()> {
        let id = self.call_ref()?.call_id.clone();
        self.stop_retx(&id, &[RetxKind::ReInvite]);
        if status < 200 {
            return Ok(());
        }
        let Some(pending) = self.call_mut()?.reinvite.take() else {
            return Ok(());
        };
        let peer = self.call_ref()?.peer;

        if !(200..300).contains(&status) {
            let ack = build_failure_ack(&pending.request, response)?.serialize()?;
            self.send(&ack, peer)?;
            self.hold_failed(pending.hold, &format!("refused ({status})"));
            return Ok(());
        }

        let branch = self.branch();
        let call = self.call_mut()?;
        let dialog = call.dialog.as_mut().ok_or("no dialog")?;
        let mut ack = dialog.build_reinvite_ack(pending.headers.cseq, &branch)?;
        add_routes(&mut ack, &call.route_set)?;
        let bytes = ack.serialize()?;
        call.reinvite_ack = Some((pending.headers, bytes.clone()));
        call.local_hold = pending.hold;
        self.send(&bytes, peer)?;
        self.lines[self.cur].media.set_hold(pending.hold);
        self.emit(Event::Hold(pending.hold));
        Ok(())
    }

    // ----- transfer ----------------------------------------------------------

    /// Asks the other side to call `target` instead of us (REFER).
    fn transfer(&mut self, target: &str) -> Result<()> {
        if self.lines[self.cur].engine.state() != State::Established {
            return Err("only a connected call can be transferred".into());
        }
        // A number is completed with the account's domain; without an
        // account, with the address of the other side.
        let domain = match self.registration.as_ref() {
            Some(registration) => registration.domain().to_owned(),
            None => self.call_ref()?.peer.to_string(),
        };
        let refer_to = server_target(target.trim(), &domain)?;
        self.send_refer(format!("<{refer_to}>"))
    }

    /// Attended transfer: the party of the selected line (the one just
    /// consulted) and the party of `other` are connected with each other.
    fn transfer_to_line(&mut self, other: usize) -> Result<()> {
        let this = self.cur;
        if other == this || other >= self.lines.len() {
            return Err("choose the other line to connect with".into());
        }
        let connected = |line: usize| self.lines[line].engine.state() == State::Established;
        if !connected(this) || !connected(other) {
            return Err("both lines must have a connected call".into());
        }

        // The call on this line is named in Replaces: the server joins the
        // other party into it in our place.
        self.ensure_dialog()?;
        let refer_to = {
            let call = self.call_ref()?;
            let config = call.dialog.as_ref().ok_or("no dialog")?.config();
            let (uri, remote_tag) = split_address(&config.remote_to);
            let (_, local_tag) = split_address(&config.local_from);
            let replaces = format!(
                "{};to-tag={};from-tag={}",
                call.call_id,
                remote_tag.ok_or("the call has no remote tag")?,
                local_tag.ok_or("the call has no local tag")?
            );
            format!("<{uri}?Replaces={}>", percent_encode(&replaces))
        };

        self.cur = other;
        let sent = self.send_refer(refer_to);
        if sent.is_ok() {
            self.call_mut()?.transfer_partner = Some(this);
        }
        self.cur = this;
        sent
    }

    /// Sends REFER with this Refer-To value on the current line.
    fn send_refer(&mut self, refer_to: String) -> Result<()> {
        self.ensure_dialog()?;
        let branch = self.branch();
        let referrer = match self.registration.as_ref() {
            Some(registration) => {
                format!("sip:{}@{}", registration.username(), registration.domain())
            }
            None => format!("sip:{}@{}", self.user, self.local),
        };
        let call = self.call_mut()?;
        if call.refer.is_some() || call.reinvite.is_some() {
            return Err("the previous request is still waiting for an answer".into());
        }
        let dialog = call.dialog.as_mut().ok_or("no dialog")?;
        let mut request = dialog.build_refer(&branch)?;
        add_routes(&mut request, &call.route_set)?;
        request.headers.insert("refer-to", refer_to)?;
        request
            .headers
            .insert("referred-by", format!("<{referrer}>"))?;

        let headers = TransactionHeaders::from_message(&Message::Request(request.clone()))?;
        let bytes = request.serialize()?;
        let (peer, id) = (call.peer, call.call_id.clone());
        call.refer = Some(headers);
        self.send(&bytes, peer)?;
        self.start_retx(RetxKind::Refer, id, bytes, peer, self.timers.t2);
        Ok(())
    }

    /// Joins all connected lines into one conversation, or separates them.
    fn set_conference(&mut self, on: bool) -> Result<()> {
        let keep = self.cur;
        self.conference = on;
        for line in &mut self.lines {
            line.media.set_conference(on);
        }
        if on {
            // Everybody talks: bring the held calls back.
            for line in 0..self.lines.len() {
                let held = self.lines[line].engine.state() == State::Established
                    && self.lines[line]
                        .call
                        .as_ref()
                        .is_some_and(|call| call.local_hold);
                if held {
                    self.cur = line;
                    if let Err(error) = self.hold(false) {
                        self.report(error.to_string());
                    }
                }
            }
            self.cur = keep;
        } else {
            self.hold_other_lines();
        }
        let _ = self.events.send(Event::Conference(on));
        Ok(())
    }

    /// NOTIFY inside the call: the server tells how the transfer is going.
    fn on_notify(
        &mut self,
        request: &Request,
        headers: &TransactionHeaders,
        source: SocketAddr,
    ) -> Result<()> {
        if !self.is_current(&headers.call_id, source) {
            return self.reply_stateless(request, source, 481, "Call/Transaction Does Not Exist");
        }
        let tag = self.call_ref()?.local_tag.clone();
        let bytes = build_response(request, 200, "OK", Some(&tag), None, None)?;
        self.remember(request, bytes.clone(), source);
        self.send(&bytes, source)?;

        let about_transfer = request
            .headers
            .get("event")
            .is_some_and(|event| event.trim().to_ascii_lowercase().starts_with("refer"));
        // The body is the status line of the new call: "SIP/2.0 200 OK".
        let status = std::str::from_utf8(&request.body)
            .ok()
            .and_then(|body| body.split_whitespace().nth(1))
            .and_then(|status| status.parse::<u16>().ok());
        let (true, Some(status)) = (about_transfer, status) else {
            return Ok(());
        };
        match status {
            0..=199 => {}
            200..=299 => {
                self.emit(Event::Transfer(TransferState::Completed));
                // Our part is over; some servers wait for us to leave. After
                // an attended transfer that is true of both calls.
                let partner = self.call_mut()?.transfer_partner.take();
                for line in [Some(self.cur), partner].into_iter().flatten() {
                    if line < self.lines.len()
                        && self.lines[line].engine.state() == State::Established
                    {
                        self.lines[line]
                            .pending
                            .push_back(Input::Command(Command::Hangup));
                    }
                }
            }
            _ => {
                self.report(format!(
                    "Transfer did not work: the new call failed ({status})"
                ));
                self.emit(Event::Transfer(TransferState::Failed(status)));
            }
        }
        Ok(())
    }

    /// The call stays as it was; the interface is told so.
    fn hold_failed(&mut self, hold: bool, why: &str) {
        let action = if hold { "Hold" } else { "Resume" };
        self.report(format!("{action} did not work: {why}"));
        let held = self.lines[self.cur]
            .call
            .as_ref()
            .is_some_and(|call| call.local_hold);
        self.emit(Event::Hold(held));
    }

    /// An INVITE from the other side inside the connected call.
    fn on_reinvite(&mut self, request: &Request, source: SocketAddr) -> Result<()> {
        let contact = self.contact();
        let call = self.call_mut()?;
        let tag = call.local_tag.clone();
        if call.reinvite.is_some() {
            // Both sides asked at once; RFC 3261 has them both back off.
            let bytes = build_response(request, 491, "Request Pending", Some(&tag), None, None)?;
            return self.send(&bytes, source);
        }

        // What the other side says about itself decides our answer.
        let theirs = sdp_direction(&request.body);
        let remote_hold = matches!(theirs, Some("sendonly" | "inactive"));
        let ours = match theirs {
            Some("sendonly") => "recvonly",
            Some("inactive") => "inactive",
            Some("recvonly") => "sendonly",
            // Both ways, or no description at all (then ours is the offer).
            _ if call.local_hold => "sendonly",
            _ => "sendrecv",
        };
        call.sdp_version += 1;
        let sdp = make_sdp(
            call.local_rtp,
            &[call.codec],
            call.dtmf_payload_type,
            call.sdp_version,
            ours,
        );
        let changed = call.remote_hold != remote_hold;
        call.remote_hold = remote_hold;

        let bytes = build_response(request, 200, "OK", Some(&tag), Some(&contact), Some(sdp))?;
        // A repeated re-INVITE (our answer was lost) gets the same answer.
        self.remember(request, bytes.clone(), source);
        self.send(&bytes, source)?;
        if changed {
            self.emit(Event::RemoteHold(remote_hold));
        }
        Ok(())
    }

    fn send_bye(&mut self) -> Result<()> {
        self.ensure_dialog()?;
        let branch = self.branch();
        let call = self.call_mut()?;
        let dialog = call.dialog.as_mut().ok_or("no dialog")?;
        let mut request = dialog.build_bye(&branch)?;
        add_rport(&mut request.headers)?;
        add_routes(&mut request, &call.route_set)?;
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
        self.trace_message(true, peer, bytes);
        self.socket.send_to(bytes, peer)?;
        Ok(())
    }

    fn trace_message(&self, outgoing: bool, peer: SocketAddr, bytes: &[u8]) {
        if self.trace {
            self.emit(Event::Trace {
                outgoing,
                peer,
                text: redact(&String::from_utf8_lossy(bytes)),
            });
        }
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
        self.cur = self.selected;
        let now = Instant::now();
        let mut expired = Vec::new();
        for retx in &mut self.retx {
            if now >= retx.deadline {
                expired.push((retx.kind, retx.call_id.clone()));
            } else if now >= retx.next {
                // A lost retransmission is repaired by the next one.
                if self.trace {
                    let _ = self.events.send(Event::Trace {
                        outgoing: true,
                        peer: retx.peer,
                        text: redact(&String::from_utf8_lossy(&retx.bytes)),
                    });
                }
                let _ = self.socket.send_to(&retx.bytes, retx.peer);
                retx.interval = (retx.interval * 2).min(retx.cap);
                retx.next = now + retx.interval;
            }
        }
        self.retx.retain(|retx| now < retx.deadline);
        self.retained.retain(|entry| now < entry.expires);

        if let Some(registration) = self.registration.as_mut() {
            if registration.keepalive_due(now) {
                // Keeps the router's mapping open; a lost one does not matter.
                let _ = self.socket.send_to(KEEPALIVE, registration.server());
            }
            let outputs = registration.poll(now);
            self.apply_registration(outputs);
        }

        for (kind, call_id) in expired {
            if kind == RetxKind::Register {
                if let Some(registration) = self
                    .registration
                    .as_mut()
                    .filter(|registration| registration.call_id() == call_id)
                {
                    let outputs = registration.on_timeout(now);
                    self.apply_registration(outputs);
                }
                continue;
            }
            // Retransmissions are kept per call: find the line it is on.
            let line = self.lines.iter().position(|line| {
                line.call
                    .as_ref()
                    .is_some_and(|call| call.call_id == call_id)
            });
            let current = line.is_some();
            if let Some(line) = line {
                self.cur = line;
            }
            if kind == RetxKind::Refer {
                if current
                    && self.lines[self.cur]
                        .call
                        .as_mut()
                        .is_some_and(|c| c.refer.take().is_some())
                {
                    self.report("Transfer did not work: no answer".to_owned());
                    self.emit(Event::Transfer(TransferState::Failed(408)));
                }
                continue;
            }
            if kind == RetxKind::ReInvite {
                if current
                    && let Some(pending) = self.lines[self.cur]
                        .call
                        .as_mut()
                        .and_then(|c| c.reinvite.take())
                {
                    self.hold_failed(pending.hold, "no answer");
                }
                continue;
            }
            let fire = current
                && matches!(
                    (kind, self.lines[self.cur].engine.state()),
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
        if !self.lines[self.cur].media_running {
            return;
        }
        if let Err(error) = self.lines[self.cur].media.pump() {
            self.report(format!("Audio failed: {error}"));
            self.lines[self.cur].media_running = false;
            self.lines[self.cur]
                .pending
                .push_back(Input::Command(Command::Hangup));
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
        self.cur = self.selected;
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
        if !data.iter().all(u8::is_ascii_whitespace) {
            // Keep-alive answers are not worth a trace line.
            self.trace_message(false, source, data);
        }
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

        // The line of this call; a new call goes to a free line. With no
        // free line the selected one answers, and it answers "busy".
        if !self.find_line(&headers.call_id, source) {
            self.cur = self.free_line().unwrap_or(self.selected);
        }
        match method {
            "ACK" => self.on_ack(&headers, source),
            "INVITE" => self.on_invite(request, source),
            "BYE" => self.on_bye(request, &headers, source),
            "CANCEL" => self.on_cancel(request, &headers, source),
            "NOTIFY" => self.on_notify(request, &headers, source),
            // Servers ask this to see whether the phone is alive.
            "OPTIONS" => {
                let tag = format!("rs-stateless-{}", self.local.port());
                let bytes = build_options_response(request, &tag)?;
                self.send(&bytes, source)
            }
            _ => self.reply_stateless(request, source, 405, "Method Not Allowed"),
        }
    }

    fn is_current(&self, call_id: &str, source: SocketAddr) -> bool {
        self.lines[self.cur]
            .call
            .as_ref()
            .is_some_and(|call| call.call_id == call_id && call.peer == source)
    }

    fn on_ack(&mut self, headers: &TransactionHeaders, source: SocketAddr) -> Result<()> {
        if !self.is_current(&headers.call_id, source) {
            return Ok(());
        }
        self.stop_retx(&headers.call_id, &[RetxKind::Final]);
        let invite_cseq = self.call_ref()?.invite_headers.cseq;
        if self.lines[self.cur].engine.state() == State::Answered && headers.cseq == invite_cseq {
            self.dispatch(Input::IncomingAck)?;
        }
        Ok(())
    }

    fn on_invite(&mut self, request: &Request, source: SocketAddr) -> Result<()> {
        if self.lines[self.cur].engine.state() != State::Idle {
            // An INVITE inside the current call changes that call (hold,
            // resume, session refresh); any other one finds the phone busy.
            let same_call = request
                .headers
                .get("call-id")
                .is_some_and(|call_id| self.is_current(call_id, source));
            if same_call && self.lines[self.cur].engine.state() == State::Established {
                return self.on_reinvite(request, source);
            }
            return self.reply_stateless(request, source, 486, "Busy Here");
        }
        if let Some(target) = self.forward_to.clone() {
            // Forwarding: the caller's server is told where to call instead.
            let (number, name) = caller_identity(header(&request.headers, "from")?);
            let domain = match self.registration.as_ref() {
                Some(registration) => registration.domain().to_owned(),
                None => source.to_string(),
            };
            let uri = server_target(&target, &domain)?;
            let tag = format!("rs-stateless-{}", self.local.port());
            let contact = format!("<{uri}>");
            let bytes = build_response(
                request,
                302,
                "Moved Temporarily",
                Some(&tag),
                Some(&contact),
                None,
            )?;
            self.send(&bytes, source)?;
            let _ = self.events.send(Event::Forwarded {
                number,
                name,
                to: target,
            });
            return Ok(());
        }
        if self.do_not_disturb {
            let (number, name) = caller_identity(header(&request.headers, "from")?);
            self.reply_stateless(request, source, 486, "Busy Here")?;
            self.emit(Event::Declined { number, name });
            return Ok(());
        }
        let (remote_rtp, codec) = match negotiate(&request.body, true) {
            Ok(agreed) => agreed,
            Err(_) => {
                return self.reply_stateless(request, source, 488, "Not Acceptable Here");
            }
        };
        if contact_uri(&request.headers).is_err() {
            return self.reply_stateless(request, source, 400, "Bad Request");
        }
        let local_rtp = match self.lines[self.cur].media.prepare(self.local.ip()) {
            Ok(address) => address,
            Err(error) => {
                self.report(format!("Cannot reserve audio port: {error}"));
                return self.reply_stateless(request, source, 500, "Server Internal Error");
            }
        };

        let tag = format!("rs-{}", self.unique());
        let mut call = Call::new(source, request.clone(), local_rtp, tag)?;
        call.remote_rtp = Some(remote_rtp);
        call.codec = codec;
        call.dtmf_payload_type = telephone_event_payload_type(&request.body);
        // The called side uses the recorded route in the order received.
        call.route_set = record_routes(&request.headers);
        self.lines[self.cur].call = Some(call);

        let trying = self.invite_response(100, "Trying", None)?;
        self.send(&trying, source)?;
        self.dispatch(Input::IncomingInvite)?;
        let (number, name) = caller_identity(header(&request.headers, "from")?);
        self.emit(Event::Caller { number, name });
        self.emit(Event::Incoming { peer: source });
        self.ring(Some(Tone::Ringtone));
        Ok(())
    }

    fn on_bye(
        &mut self,
        request: &Request,
        headers: &TransactionHeaders,
        source: SocketAddr,
    ) -> Result<()> {
        let live = matches!(
            self.lines[self.cur].engine.state(),
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
            && self.lines[self.cur]
                .call
                .as_ref()
                .is_some_and(|call| call.invite_headers.branch == headers.branch)
            && matches!(
                self.lines[self.cur].engine.state(),
                State::Incoming | State::Answered
            );
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
        if self.on_register_response(response, &headers, source) {
            return Ok(());
        }
        if !self.find_line(&headers.call_id, source) {
            return Ok(());
        }
        let Some(call) = self.lines[self.cur].call.as_ref() else {
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
            "REFER"
                if call
                    .refer
                    .as_ref()
                    .is_some_and(|sent| sent.matches_response(&headers)) =>
            {
                if status < 200 {
                    return Ok(());
                }
                self.stop_retx(&headers.call_id, &[RetxKind::Refer]);
                self.call_mut()?.refer = None;
                if (200..300).contains(&status) {
                    self.emit(Event::Transfer(TransferState::Accepted));
                } else {
                    self.report(format!("Transfer did not work: refused ({status})"));
                    self.emit(Event::Transfer(TransferState::Failed(status)));
                }
                Ok(())
            }
            "INVITE"
                if call
                    .reinvite
                    .as_ref()
                    .is_some_and(|sent| sent.headers.matches_response(&headers)) =>
            {
                self.on_reinvite_response(response, status)
            }
            "INVITE"
                if status >= 200
                    && call
                        .reinvite_ack
                        .as_ref()
                        .is_some_and(|(sent, _)| sent.matches_response(&headers)) =>
            {
                // Our ACK for the hold request was lost; repeat it.
                if let Some((_, ack)) = call.reinvite_ack.as_ref() {
                    self.send(ack, source)?;
                }
                Ok(())
            }
            "INVITE"
                if status >= 300
                    && call
                        .superseded
                        .as_ref()
                        .is_some_and(|(sent, _)| sent.matches_response(&headers)) =>
            {
                // The ACK for the challenged INVITE was lost; repeat it.
                if let Some((_, ack)) = call.superseded.as_ref() {
                    self.send(ack, source)?;
                }
                Ok(())
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
                if matches!(status, 401 | 407) && self.retry_bye_with_credentials(response)? {
                    return Ok(());
                }
                self.stop_retx(&headers.call_id, &[RetxKind::Bye]);
                if (200..300).contains(&status)
                    && let Some(dialog) = self.call_mut()?.dialog.as_mut()
                {
                    // A refused state change here only means the BYE was not sent by us.
                    let _ = dialog.finish_local_bye();
                }
                if self.lines[self.cur].engine.state() == State::Ending {
                    self.dispatch(Input::ByeResponse(status))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn on_invite_response(&mut self, response: &Response, status: u16) -> Result<()> {
        let id = self.call_ref()?.call_id.clone();
        let state = self.lines[self.cur].engine.state();

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
                self.ring(Some(Tone::Ringback));
            }
            return Ok(());
        }

        if matches!(status, 401 | 407)
            && state == State::Outgoing
            && self.retry_with_credentials(response)?
        {
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
        if peer.port() == 0 || peer.ip().is_unspecified() || peer.is_ipv4() != self.local.is_ipv4()
        {
            return Err("invalid destination address".into());
        }
        let (user, local) = (self.user.clone(), self.local);
        self.start_call(
            peer,
            format!("sip:{user}@{peer}"),
            format!("sip:{user}@{local}"),
            format!("sip:{user}@{local}"),
            false,
        )
    }

    /// `IP:port` is called directly; anything else is a number or user on the
    /// SIP server the account is registered on.
    fn dial_target(&mut self, text: &str) -> Result<()> {
        let text = text.trim();
        if let Ok(peer) = text.parse::<SocketAddr>() {
            return self.dial(peer);
        }
        let registration = self
            .registration
            .as_ref()
            .filter(|registration| registration.is_bound())
            .ok_or("register on a SIP server before calling a number")?;
        let target = server_target(text, registration.domain())?;
        let peer = registration.server();
        let from_uri = format!("sip:{}@{}", registration.username(), registration.domain());
        let contact_uri = format!(
            "sip:{}@{}",
            registration.username(),
            registration.contact_address()
        );
        self.start_call(peer, target, from_uri, contact_uri, true)
    }

    fn start_call(
        &mut self,
        peer: SocketAddr,
        target: String,
        from_uri: String,
        contact_uri: String,
        through_server: bool,
    ) -> Result<()> {
        if self.lines[self.cur].engine.state() != State::Idle {
            return Err("a call is already in progress".into());
        }
        let local_rtp = self.lines[self.cur].media.prepare(self.local.ip())?;
        let local = self.local;
        let call_id = format!("{}@{}", self.unique(), local.ip());
        let branch = self.branch();
        let from_tag = self.unique();
        let mut invite = InviteRequest::new(
            target.clone(),
            from_uri.clone(),
            target.clone(),
            contact_uri.clone(),
            local.to_string(),
            call_id,
            branch,
            from_tag.clone(),
        )
        .body(offer_sdp(local_rtp).into_bytes())
        .build()?;
        add_rport(&mut invite.headers)?;

        let local_tag = format!("rs-{}", self.unique());
        let mut call = Call::new(peer, invite, local_rtp, local_tag)?;
        if through_server {
            call.server = Some(ServerCall {
                target,
                from_uri,
                contact_uri,
                from_tag,
                authorized: false,
            });
        }
        self.lines[self.cur].call = Some(call);
        self.dispatch(Input::Command(Command::Dial))?;
        // A new call takes over the conversation.
        self.hold_other_lines();
        Ok(())
    }

    /// A few servers ask for the password again when a call is ended. The
    /// BYE is then sent once more with credentials. Returns false when
    /// there is nothing to answer with, and the response ends the call.
    fn retry_bye_with_credentials(&mut self, response: &Response) -> Result<bool> {
        let branch = self.branch();
        let local = self.local;
        let Some(call) = self.lines[self.cur].call.as_ref() else {
            return Ok(false);
        };
        let (Some(previous), Some(sent), false) = (
            call.bye_request.as_ref(),
            call.bye_headers.as_ref(),
            call.bye_authorized,
        ) else {
            return Ok(false);
        };
        let Some(registration) = self.registration.as_ref() else {
            return Ok(false);
        };
        let Ok((name, value)) = registration.authorize(response, "BYE", &previous.target) else {
            return Ok(false);
        };

        // The same BYE as a new transaction: next CSeq, new branch.
        let mut request = previous.clone();
        request
            .headers
            .insert("cseq", format!("{} BYE", sent.cseq + 1))?;
        request
            .headers
            .insert("via", format!("SIP/2.0/UDP {local};branch={branch};rport"))?;
        request.headers.insert(name, value)?;
        let headers = TransactionHeaders::from_message(&Message::Request(request.clone()))?;
        let bytes = request.serialize()?;
        let (peer, id) = (call.peer, call.call_id.clone());

        self.stop_retx(&id, &[RetxKind::Bye]);
        let call = self.call_mut()?;
        call.bye_request = Some(request);
        call.bye_headers = Some(headers);
        call.bye_authorized = true;
        self.send(&bytes, peer)?;
        self.start_retx(RetxKind::Bye, id, bytes, peer, self.timers.t2);
        Ok(true)
    }

    /// Answers a 401/407 to an INVITE sent through the server: acknowledges
    /// it and sends the INVITE again with credentials, once per call. Returns
    /// false when the response must end the call instead.
    fn retry_with_credentials(&mut self, response: &Response) -> Result<bool> {
        let Some(call) = self.lines[self.cur].call.as_ref() else {
            return Ok(false);
        };
        let Some(server) = call.server.as_ref() else {
            return Ok(false);
        };
        // A second challenge means wrong credentials; a hang-up already given
        // must not be overtaken by a new INVITE.
        if server.authorized || call.reason.is_some() || call.cancel_request.is_some() {
            return Ok(false);
        }
        let Some(registration) = self.registration.as_ref() else {
            return Ok(false);
        };
        let Ok((name, value)) = registration.authorize(response, "INVITE", &server.target) else {
            return Ok(false);
        };

        let ack = build_failure_ack(&call.invite, response)?.serialize()?;
        let (peer, id) = (call.peer, call.call_id.clone());
        let old_headers = call.invite_headers.clone();
        let mut invite = InviteRequest::new(
            server.target.clone(),
            server.from_uri.clone(),
            server.target.clone(),
            server.contact_uri.clone(),
            self.local.to_string(),
            id.clone(),
            format!(
                "z9hG4bK-{}-auth",
                old_headers.branch.trim_start_matches("z9hG4bK-")
            ),
            server.from_tag.clone(),
        )
        .cseq(old_headers.cseq + 1)
        .body(offer_sdp(call.local_rtp).into_bytes())
        .build()?;
        invite.headers.insert(name, value)?;
        add_rport(&mut invite.headers)?;
        let invite_headers = TransactionHeaders::from_message(&Message::Request(invite.clone()))?;

        self.send(&ack, peer)?;
        self.stop_retx(&id, &[RetxKind::Invite]);
        let call = self.call_mut()?;
        call.superseded = Some((old_headers, ack));
        call.invite = invite;
        call.invite_headers = invite_headers;
        if let Some(server) = call.server.as_mut() {
            server.authorized = true;
        }
        self.execute(Action::SendInvite)?;
        Ok(true)
    }
}

// ----- message helpers -----------------------------------------------------

/// Hides the Digest `response` value: with the rest of the message it would
/// allow guessing the password offline.
fn redact(text: &str) -> String {
    const MARKER: &str = "response=\"";
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(MARKER) {
        // `response=` must be a parameter of its own, not the tail of `cresponse=`.
        let own = rest[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric());
        let value = start + MARKER.len();
        output.push_str(&rest[..value]);
        match rest[value..].find('"') {
            Some(end) if own => {
                output.push_str("<hidden>");
                rest = &rest[value + end..];
            }
            _ => rest = &rest[value..],
        }
    }
    output.push_str(rest);
    output
}

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
    if request.method.as_str() == "INVITE" {
        // A proxy that recorded itself must see its route in the answer.
        for route in request.headers.get_all("record-route") {
            builder = builder.append_header("record-route", route)?;
        }
    }
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

/// Answer to OPTIONS: the phone is here, and this is what it understands.
fn build_options_response(request: &Request, to_tag: &str) -> Result<Vec<u8>> {
    let to = header(&request.headers, "to")?;
    let to = if has_tag(to) {
        to.to_owned()
    } else {
        format!("{to};tag={to_tag}")
    };
    let mut builder = ResponseBuilder::new(200, "OK");
    for via in request.headers.get_all("via") {
        builder = builder.append_header("via", via)?;
    }
    let response = builder
        .header("from", header(&request.headers, "from")?)?
        .header("to", to)?
        .header("call-id", header(&request.headers, "call-id")?)?
        .header("cseq", header(&request.headers, "cseq")?)?
        .header("allow", "INVITE, ACK, CANCEL, BYE, OPTIONS, NOTIFY")?
        .header("accept", "application/sdp")?
        .build()?;
    Ok(response.serialize()?)
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

/// Number (user part of the URI) and display name from a From value such as
/// `"Ivan Petrov" <sip:4010@pbx.example.com>;tag=1`.
fn caller_identity(from: &str) -> (String, String) {
    let (uri, _) = split_address(from);
    let address = uri
        .strip_prefix("sips:")
        .or_else(|| uri.strip_prefix("sip:"))
        .unwrap_or(&uri);
    let number = address.split(['@', ';']).next().unwrap_or("").to_owned();
    let name = match from.find('<') {
        Some(start) => from[..start].trim().trim_matches('"').trim().to_owned(),
        None => String::new(),
    };
    // A name that only repeats the number says nothing.
    let name = if name == number { String::new() } else { name };
    (number, name)
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

/// Escapes a value for use inside a URI header parameter (`?Replaces=...`).
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Record-Route entries in header order; one header may list several.
fn record_routes(headers: &Headers) -> Vec<String> {
    let mut routes = Vec::new();
    for value in headers.get_all("record-route") {
        let mut depth = 0_u32;
        let mut start = 0;
        for (index, character) in value.char_indices() {
            match character {
                '<' => depth += 1,
                '>' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    routes.push(value[start..index].trim().to_owned());
                    start = index + 1;
                }
                _ => {}
            }
        }
        routes.push(value[start..].trim().to_owned());
    }
    routes.retain(|route| !route.is_empty());
    routes
}

fn add_routes(request: &mut Request, routes: &[String]) -> Result<()> {
    for route in routes {
        request.headers.append("route", route.as_str())?;
    }
    Ok(())
}

/// Request-URI for a number or user typed by the user: `100`, `100@host`
/// or `sip:100@host`.
fn server_target(text: &str, domain: &str) -> Result<String> {
    let text = text.strip_prefix("sip:").unwrap_or(text);
    let (user, host) = match text.split_once('@') {
        Some((user, host)) => (user, host),
        None => (text, domain),
    };
    let bad = |part: &str| {
        part.is_empty()
            || part.contains(|character: char| {
                character.is_whitespace() || matches!(character, '@' | '<' | '>' | ';' | ',')
            })
    };
    if bad(user) || bad(host) || user.contains(':') {
        return Err("invalid number or SIP address".into());
    }
    Ok(format!("sip:{user}@{host}"))
}

/// Payload type this phone proposes for telephone-event (keypad digits).
const OFFERED_DTMF_PAYLOAD_TYPE: u8 = 101;

/// The first offer of a call: every codec this phone has, in its order of
/// preference, and telephone-event.
fn offer_sdp(address: SocketAddr) -> String {
    make_sdp(
        address,
        &Codec::PREFERRED,
        Some(OFFERED_DTMF_PAYLOAD_TYPE),
        1,
        "sendrecv",
    )
}

/// Audio with the given codecs, plus telephone-event with the given payload
/// type when the digits may be sent as RFC 4733 events. `version` must grow
/// whenever the description changes; `direction` is `sendrecv`, `sendonly`
/// (hold), `recvonly` or `inactive`.
fn make_sdp(
    address: SocketAddr,
    codecs: &[Codec],
    dtmf: Option<u8>,
    version: u32,
    direction: &str,
) -> String {
    let family = if address.is_ipv4() { "IP4" } else { "IP6" };
    let ip = address.ip();
    let port = address.port();
    let (dtmf_format, dtmf_lines) = match dtmf {
        Some(payload) => (
            format!(" {payload}"),
            format!("a=rtpmap:{payload} telephone-event/8000\r\na=fmtp:{payload} 0-16\r\n"),
        ),
        None => (String::new(), String::new()),
    };
    let formats: Vec<String> = codecs
        .iter()
        .map(|codec| codec.payload_type().to_string())
        .collect();
    let formats = formats.join(" ");
    let codec_lines: String = codecs
        .iter()
        .map(|codec| format!("a=rtpmap:{} {}\r\n", codec.payload_type(), codec.rtpmap()))
        .collect();
    format!(
        "v=0\r\no=phone 1 {version} IN {family} {ip}\r\ns=Rust direct call\r\n\
         c=IN {family} {ip}\r\nt=0 0\r\nm=audio {port} RTP/AVP {formats}{dtmf_format}\r\n\
         {codec_lines}{dtmf_lines}a={direction}\r\n"
    )
}

/// Direction of the audio in an SDP body, as the sender of the body sees it:
/// `sendrecv`, `sendonly`, `recvonly` or `inactive`. An address of 0.0.0.0
/// is the old way of saying "on hold" and reads as `sendonly`.
fn sdp_direction(body: &[u8]) -> Option<&'static str> {
    let sdp = Sdp::parse(std::str::from_utf8(body).ok()?).ok()?;
    let audio = sdp.media(&MediaKind::Audio)?;
    let unspecified = audio
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .and_then(|connection| connection.address.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_unspecified());
    let named = ["sendonly", "recvonly", "inactive", "sendrecv"]
        .into_iter()
        .find(|name| audio.attribute(name).is_some() || sdp.attribute(name).is_some());
    match named {
        Some("sendrecv") | None if unspecified => Some("sendonly"),
        Some(name) => Some(name),
        None => Some("sendrecv"),
    }
}

/// Payload type of 8 kHz telephone-event in an SDP body, if it is offered.
fn telephone_event_payload_type(body: &[u8]) -> Option<u8> {
    let sdp = Sdp::parse(std::str::from_utf8(body).ok()?).ok()?;
    let audio = sdp.media(&MediaKind::Audio)?;
    audio
        .attributes
        .iter()
        .filter(|attribute| attribute.name.eq_ignore_ascii_case("rtpmap"))
        .filter_map(|attribute| attribute.value.as_deref()?.split_once(' '))
        .find(|(_, encoding)| encoding.trim().eq_ignore_ascii_case("telephone-event/8000"))
        .and_then(|(payload, _)| payload.trim().parse::<u8>().ok())
        // The type must also be listed on the m= line and be a dynamic one.
        .filter(|payload| {
            (96..=127).contains(payload)
                && audio
                    .formats
                    .iter()
                    .any(|format| *format == payload.to_string())
        })
}

/// Audio address from an SDP body; see [`negotiate`].
#[cfg(test)]
fn media_address(body: &[u8]) -> Result<SocketAddr> {
    negotiate(body, true).map(|(address, _)| address)
}

/// Audio address and codec from an SDP body. Requires RTP/AVP audio with at
/// least one codec this phone has. `is_offer` says whose preference decides:
/// ours for an offer, the other side's for an answer.
fn negotiate(body: &[u8], is_offer: bool) -> Result<(SocketAddr, Codec)> {
    let sdp = Sdp::parse(std::str::from_utf8(body)?)?;
    let audio = sdp.media(&MediaKind::Audio).ok_or("no audio in SDP")?;
    if audio.port == 0 || audio.protocol != "RTP/AVP" {
        return Err("audio must be enabled RTP/AVP".into());
    }
    let codec = Codec::choose(&audio.formats, is_offer)
        .ok_or("no common codec: this phone has G.711 (PCMU and PCMA) only")?;
    let connection = audio
        .connection
        .as_ref()
        .or(sdp.connection.as_ref())
        .ok_or("no media connection address")?;
    let ip: IpAddr = connection.address.parse()?;
    if ip.is_unspecified() {
        return Err("hold or unspecified media address is not supported".into());
    }
    Ok((SocketAddr::new(ip, audio.port), codec))
}

#[cfg(test)]
mod tests {
    use super::{
        Codec, caller_identity, make_sdp, media_address, negotiate, offer_sdp, redact,
        sdp_direction, telephone_event_payload_type,
    };

    #[test]
    fn caller_number_and_name_are_read_from_the_from_header() {
        let identity = |from: &str| caller_identity(from);
        let pair = |number: &str, name: &str| (number.to_owned(), name.to_owned());
        assert_eq!(
            identity("\"Иван Петров\" <sip:4010@pbx.example.com>;tag=1"),
            pair("4010", "Иван Петров")
        );
        assert_eq!(
            identity("Taxi <sip:+74959999999@80.74.27.49:1560;user=phone>;tag=a"),
            pair("+74959999999", "Taxi")
        );
        assert_eq!(identity("<sip:4010@pbx>;tag=1"), pair("4010", ""));
        assert_eq!(identity("sip:alice@pbx;tag=1"), pair("alice", ""));
        // A display name equal to the number is dropped.
        assert_eq!(identity("\"4010\" <sip:4010@pbx>;tag=1"), pair("4010", ""));
        assert_eq!(identity("<sips:bob@pbx>"), pair("bob", ""));
    }

    #[test]
    fn first_offer_lists_both_codecs_and_an_answer_names_one() {
        let address = "192.0.2.5:40000".parse().unwrap();
        let offer = offer_sdp(address);
        assert!(
            offer.contains("m=audio 40000 RTP/AVP 0 8 101\r\n"),
            "{offer}"
        );
        assert!(offer.contains("a=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n"));

        let answer = make_sdp(address, &[Codec::Pcma], None, 1, "sendrecv");
        assert!(answer.contains("m=audio 40000 RTP/AVP 8\r\n"), "{answer}");
        assert!(answer.contains("a=rtpmap:8 PCMA/8000\r\n"));
        assert!(!answer.contains("PCMU"));
    }

    #[test]
    fn codec_is_agreed_from_what_the_other_side_lists() {
        let sdp = |formats: &str| {
            format!(
                "v=0\r\no=x 1 1 IN IP4 192.0.2.1\r\ns=x\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\n\
                 m=audio 4000 RTP/AVP {formats}\r\n"
            )
        };
        let address = "192.0.2.1:4000".parse().unwrap();
        let agreed = |formats: &str, is_offer: bool| {
            negotiate(sdp(formats).as_bytes(), is_offer).map_err(|error| error.to_string())
        };
        // An offer with only PCMA is accepted with PCMA.
        assert_eq!(agreed("8 101", true), Ok((address, Codec::Pcma)));
        // Offered both: our preference. Answered both: their first.
        assert_eq!(agreed("8 0", true), Ok((address, Codec::Pcmu)));
        assert_eq!(agreed("8 0", false), Ok((address, Codec::Pcma)));
        // Nothing in common.
        assert!(
            agreed("18 9", true)
                .unwrap_err()
                .contains("no common codec")
        );
        assert!(agreed("18 9", false).is_err());
    }

    #[test]
    fn direction_and_version_are_written_and_read_back() {
        let address = "192.0.2.5:40000".parse().unwrap();
        for direction in ["sendrecv", "sendonly", "recvonly", "inactive"] {
            let sdp = make_sdp(address, &Codec::PREFERRED, Some(101), 7, direction);
            assert!(sdp.contains("o=phone 1 7 IN IP4 192.0.2.5\r\n"));
            assert_eq!(sdp_direction(sdp.as_bytes()), Some(direction));
        }
        // No direction line means both ways; 0.0.0.0 is the old hold.
        let plain = "v=0\r\no=x 1 1 IN IP4 192.0.2.1\r\ns=x\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\n\
                     m=audio 4000 RTP/AVP 0\r\n";
        assert_eq!(sdp_direction(plain.as_bytes()), Some("sendrecv"));
        let old_hold = plain.replace("c=IN IP4 192.0.2.1", "c=IN IP4 0.0.0.0");
        assert_eq!(sdp_direction(old_hold.as_bytes()), Some("sendonly"));
        // A session-level direction applies to the audio.
        let session = plain.replace("t=0 0\r\n", "t=0 0\r\na=inactive\r\n");
        assert_eq!(sdp_direction(session.as_bytes()), Some("inactive"));
        assert_eq!(sdp_direction(b""), None);
    }

    #[test]
    fn offer_lists_telephone_event_and_stays_valid_pcmu() {
        let address = "192.0.2.5:40000".parse().unwrap();
        let offer = make_sdp(address, &[Codec::Pcmu], Some(101), 1, "sendrecv");
        assert!(offer.contains("m=audio 40000 RTP/AVP 0 101\r\n"));
        assert!(offer.contains("a=rtpmap:101 telephone-event/8000\r\n"));
        assert!(offer.contains("a=fmtp:101 0-16\r\n"));
        // Our own offer is something we would accept ourselves.
        assert_eq!(media_address(offer.as_bytes()).unwrap(), address);
        assert_eq!(telephone_event_payload_type(offer.as_bytes()), Some(101));

        let plain = make_sdp(address, &[Codec::Pcmu], None, 1, "sendrecv");
        assert!(plain.contains("m=audio 40000 RTP/AVP 0\r\n"));
        assert!(!plain.contains("telephone-event"));
        assert_eq!(telephone_event_payload_type(plain.as_bytes()), None);
    }

    #[test]
    fn telephone_event_type_is_read_from_the_other_side() {
        let sdp = |formats: &str, lines: &str| {
            format!(
                "v=0\r\no=x 1 1 IN IP4 192.0.2.1\r\ns=x\r\nc=IN IP4 192.0.2.1\r\nt=0 0\r\n\
                 m=audio 4000 RTP/AVP {formats}\r\na=rtpmap:0 PCMU/8000\r\n{lines}"
            )
        };
        let found = |formats: &str, lines: &str| {
            telephone_event_payload_type(sdp(formats, lines).as_bytes())
        };
        assert_eq!(
            found("0 96", "a=rtpmap:96 telephone-event/8000\r\n"),
            Some(96)
        );
        assert_eq!(
            found(
                "0 8 101",
                "a=rtpmap:8 PCMA/8000\r\na=rtpmap:101 TELEPHONE-EVENT/8000\r\n"
            ),
            Some(101)
        );
        // Wideband events, a type missing from the m= line, a static type.
        assert_eq!(
            found("0 101", "a=rtpmap:101 telephone-event/16000\r\n"),
            None
        );
        assert_eq!(found("0", "a=rtpmap:101 telephone-event/8000\r\n"), None);
        assert_eq!(found("0 8", "a=rtpmap:8 telephone-event/8000\r\n"), None);
        assert_eq!(found("0", ""), None);
        assert_eq!(telephone_event_payload_type(b"not sdp"), None);
    }

    #[test]
    fn digest_response_is_hidden_and_everything_else_is_kept() {
        let text = "Authorization: Digest username=\"alice\", nonce=\"n1\", \
                    response=\"0123456789abcdef\", cnonce=\"c1\"\r\n\
                    Proxy-Authorization: Digest response=\"feedface\"\r\n";
        let hidden = redact(text);
        assert!(!hidden.contains("0123456789abcdef"));
        assert!(!hidden.contains("feedface"));
        assert_eq!(hidden.matches("response=\"<hidden>\"").count(), 2);
        assert!(hidden.contains("username=\"alice\""));
        assert!(hidden.contains("nonce=\"n1\""));
        assert!(hidden.contains("cnonce=\"c1\""));
    }

    #[test]
    fn text_without_credentials_is_unchanged() {
        let text = "SIP/2.0 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"pbx\"\r\n";
        assert_eq!(redact(text), text);
        assert_eq!(redact("response=\"unterminated"), "response=\"unterminated");
    }
}
