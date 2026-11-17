//! Interactive direct-UDP phone on top of the call engine and network runner.
//!
//! One program handles both incoming and outgoing calls. It prints one status
//! line per event so scripts and, later, the GUI can follow it.

use microsip_core::call_media::{AudioMedia, NullMedia, ToneMedia, ToneReport};
use microsip_core::call_runner::{CallHandle, EndReason, Event, Timers, UiCommand, spawn_lines};
use microsip_core::codec::Codec;
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::srtp::MediaEncryption;
use microsip_core::stream::SipTransport;
use std::error::Error;
use std::io::{self, BufRead};
use std::net::SocketAddr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const HELP: &str = "Usage: phone-engine.exe LOCAL_IP:SIP_PORT [--no-audio] [--trace]\n\
       [--codecs=LIST] [--tone=HZ] [--transport=udp|tcp|tls]\n\
       [--proxy=HOST[:PORT]] [--tls-insecure] [--srtp=off|optional|required]\n\
       [--stun=HOST[:PORT]]\n\
Port 0 picks a free port; the chosen address is printed on the first line.\n\
Commands: call IP:PORT | call NUMBER | answer | reject | hangup | status | quit\n\
          register SERVER[:PORT] USER PASSWORD [DOMAIN] | unregister\n\
          dtmf DIGITS | mute | unmute | hold | resume\n\
call IP:PORT dials directly. register keeps the account registered on a SIP\n\
server; after that the server can send calls here and call NUMBER (or\n\
USER@DOMAIN) dials through it.\n\
--no-audio runs without sound devices. --trace prints every SIP message\n\
(the Digest response value is hidden).\n\
--codecs=opus,G722,PCMA sets the codecs and their order (PCMU, PCMA, G722,\n\
opus, G729). --tone=440 needs no sound devices: it sends a tone of that\n\
pitch and prints what was heard when the call ends.\n\
--transport sets how register talks to the server (default udp); --proxy\n\
sends everything through that address instead; --tls-insecure accepts a\n\
TLS certificate that cannot be checked. --srtp encrypts the audio of calls:\n\
optional when the other side can, required always (default off). --stun\n\
asks that server for the public address of each call's audio.";

const COMMANDS: &str = "Commands: call IP:PORT | call NUMBER | answer | reject | hangup | status | quit | \
register SERVER[:PORT] USER PASSWORD [DOMAIN] | unregister | dtmf DIGITS | mute | unmute | hold | resume | transfer NUMBER | dnd on|off | line 1|2 | join | conference on|off | forward NUMBER|off";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Calling,
    Incoming,
    Answering,
    Connected,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Calling => "Calling",
            Self::Incoming => "Incoming",
            Self::Answering => "Answering",
            Self::Connected => "Connected",
        }
    }
}

struct Phone {
    handle: CallHandle,
    local: SocketAddr,
    /// State of each line; `state` and `set_state` work on one of them.
    states: Vec<State>,
    /// The line the events being printed are about.
    event_line: usize,
    /// The line the typed commands go to.
    selected: usize,
    registration: String,
    registered: bool,
    quitting: bool,
    /// How `register` reaches the server: --transport, --proxy,
    /// --tls-insecure.
    transport: SipTransport,
    proxy: String,
    tls_verify: bool,
    /// With --tone: what the test tone's receiver heard in the last call.
    tone: Option<Arc<Mutex<ToneReport>>>,
}

fn describe(reason: EndReason) -> String {
    match reason {
        EndReason::Completed => "completed".to_owned(),
        EndReason::Rejected(status) => format!("rejected ({status})"),
        EndReason::Cancelled => "cancelled".to_owned(),
        EndReason::Timeout => "timeout".to_owned(),
        EndReason::Failed => "failed".to_owned(),
    }
}

const LINES: usize = 2;

impl Phone {
    /// State of the line in question: while an event is handled that is the
    /// event's line, otherwise the selected one.
    fn state(&self) -> State {
        self.states[self.event_line]
    }

    fn set_state(&mut self, state: State) {
        self.states[self.event_line] = state;
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Line(line) => {
                self.event_line = line.min(LINES - 1);
                println!("--- line {} ---", line + 1);
            }
            Event::Selected(line) => {
                self.selected = line.min(LINES - 1);
                println!("Selected line {}.", line + 1);
            }
            Event::Conference(true) => println!("Conference on."),
            Event::Conference(false) => println!("Conference off."),
            Event::Forwarded { number, name, to } => {
                println!("Forwarded to {to}: {number} {name}");
            }
            Event::Incoming { peer } => {
                self.set_state(State::Incoming);
                println!("Incoming call from {peer}. Type answer or reject.");
            }
            Event::Ringing => println!("Remote is ringing."),
            Event::Established => {
                self.set_state(State::Connected);
                println!("Connected.");
                if self.quitting {
                    // A hangup sent before the ACK was refused; repeat it now.
                    self.handle.send(UiCommand::Hangup);
                }
            }
            Event::MediaStats { sent, received } => {
                println!("RTP sent: {sent}; RTP received: {received}");
            }
            Event::AudioQuality(quality) => println!("Audio quality: {quality}"),
            Event::Caller { number, name } => println!("Caller: {number} {name}"),
            Event::Recording(Some(path)) => println!("Recording to {}", path.display()),
            Event::Recording(None) => println!("Recording stopped."),
            Event::Transfer(state) => println!("Transfer: {state:?}"),
            Event::Declined { number, name } => {
                println!("Declined (do not disturb): {number} {name}")
            }
            Event::Hold(true) => println!("Call is on hold."),
            Event::Hold(false) => println!("Call resumed."),
            Event::RemoteHold(true) => println!("The other side put the call on hold."),
            Event::RemoteHold(false) => println!("The other side resumed the call."),
            Event::Ended(reason) => {
                self.set_state(State::Idle);
                println!("Call ended: {}", describe(reason));
                if let Some(report) = self.tone.as_ref()
                    && let Ok(mut report) = report.lock()
                {
                    if report.seconds > 0.0 {
                        println!(
                            "Tone heard: {:.0} Hz, level {:.2}, {:.1} s",
                            report.frequency, report.level, report.seconds
                        );
                    }
                    *report = ToneReport::default();
                }
            }
            Event::Codec(codec) => println!("Codec: {codec}"),
            Event::Encrypted(true) => println!("Audio is encrypted (SRTP)."),
            Event::Encrypted(false) => {}
            Event::LocalAddress(address) => {
                self.local = address;
                println!("Moved to {address}");
            }
            Event::Registration(state) => {
                self.registration = state.to_string();
                self.registered = matches!(state, RegistrationState::Registered { .. });
                println!("Registration: {state}");
            }
            Event::Trace {
                outgoing,
                peer,
                text,
            } => {
                let arrow = if outgoing { "sent to" } else { "received from" };
                println!("--- SIP {arrow} {peer} ---");
                println!("{}", text.trim_end());
                println!("--- end ---");
            }
            Event::Error(message) => println!("Error: {message}"),
        }
    }

    fn on_line(&mut self, line: &str) {
        // Typed commands are about the selected line.
        let event_line = std::mem::replace(&mut self.event_line, self.selected);
        self.on_typed(line);
        self.event_line = event_line;
    }

    fn on_typed(&mut self, line: &str) {
        let mut parts = line.split_whitespace();
        match parts.next() {
            None => {}
            Some("call") => self.call(parts.next()),
            // The ringing call is answered wherever it is; the phone then
            // selects its line and holds the other call.
            Some("answer") => match self.states.iter().position(|s| *s == State::Incoming) {
                Some(line) => {
                    self.states[line] = State::Answering;
                    self.handle.send(UiCommand::Answer);
                }
                None => println!("No incoming call to answer."),
            },
            Some("reject") => {
                if self.states.contains(&State::Incoming) {
                    self.handle.send(UiCommand::Reject);
                } else {
                    println!("No ringing call to reject.");
                }
            }
            Some("hangup") => {
                if self.state() == State::Idle {
                    println!("No active call.");
                } else {
                    self.handle.send(UiCommand::Hangup);
                }
            }
            Some("status") => println!(
                "State: {}; local: {}; registration: {}",
                self.state().name(),
                self.local,
                self.registration
            ),
            Some("register") => {
                let (server, user, password) = (parts.next(), parts.next(), parts.next());
                let (Some(server), Some(user), Some(password)) = (server, user, password) else {
                    println!("Usage: register SERVER[:PORT] USER PASSWORD [DOMAIN]");
                    return;
                };
                let account = Account::new(server, user, password)
                    .with_domain(parts.next().unwrap_or(""))
                    .with_transport(self.transport)
                    .with_proxy(self.proxy.clone())
                    .with_tls_verify(self.tls_verify);
                self.handle.send(UiCommand::Register(account));
            }
            Some("dtmf") => match parts.next() {
                Some(digits) if self.state() == State::Connected => {
                    for digit in digits.chars() {
                        self.handle.send(UiCommand::Dtmf(digit));
                    }
                    println!("Sending digits {digits}");
                }
                Some(_) => println!("Digits can be sent only during a connected call."),
                None => println!("Usage: dtmf DIGITS"),
            },
            Some(word @ ("mute" | "unmute")) => {
                let muted = word == "mute";
                self.handle.send(UiCommand::Mute(muted));
                println!("Microphone {}.", if muted { "off" } else { "on" });
            }
            Some(word @ ("hold" | "resume")) => {
                if self.state() == State::Connected {
                    self.handle.send(UiCommand::Hold(word == "hold"));
                } else {
                    println!("Only a connected call can be put on hold.");
                }
            }
            Some("transfer") => match parts.next() {
                Some(target) if self.state() == State::Connected => {
                    println!("Transferring to {target}");
                    self.handle.send(UiCommand::Transfer(target.to_owned()));
                }
                Some(_) => println!("Only a connected call can be transferred."),
                None => println!("Usage: transfer NUMBER"),
            },
            Some("line") => match parts.next().and_then(|text| text.parse::<usize>().ok()) {
                Some(line) if (1..=LINES).contains(&line) => {
                    self.handle.send(UiCommand::SelectLine(line - 1));
                }
                _ => println!("Usage: line 1|2"),
            },
            Some("conference") => {
                let on = parts.next() != Some("off");
                self.handle.send(UiCommand::Conference(on));
            }
            Some("join") => {
                // Connects the two calls with each other and leaves both.
                let other = (self.selected + 1) % LINES;
                self.handle.send(UiCommand::TransferToLine(other));
                println!(
                    "Connecting line {} with line {}",
                    self.selected + 1,
                    other + 1
                );
            }
            Some("forward") => match parts.next() {
                Some("off") | None => {
                    self.handle.send(UiCommand::Forward(None));
                    println!("Forwarding is off.");
                }
                Some(target) => {
                    self.handle
                        .send(UiCommand::Forward(Some(target.to_owned())));
                    println!("Forwarding to {target}.");
                }
            },
            Some("dnd") => {
                let on = parts.next() == Some("on");
                self.handle.send(UiCommand::DoNotDisturb(on));
                println!("Do not disturb is {}.", if on { "on" } else { "off" });
            }
            Some("unregister") => {
                self.handle.send(UiCommand::Unregister);
            }
            Some("quit") => self.begin_quit(),
            Some(_) => println!("{COMMANDS}"),
        }
    }

    fn call(&mut self, argument: Option<&str>) {
        if self.state() != State::Idle {
            // The phone puts a new call on a free line and holds the other.
            let Some(free) = self.states.iter().position(|state| *state == State::Idle) else {
                println!("A call is already in progress.");
                return;
            };
            self.selected = free;
            self.event_line = free;
        }
        let Some(text) = argument else {
            println!("Usage: call IP:PORT or call NUMBER");
            return;
        };
        match text.parse::<SocketAddr>() {
            Ok(peer) if !peer.ip().is_unspecified() && peer.port() != 0 => {
                self.set_state(State::Calling);
                println!("Calling {peer}");
                self.handle.send(UiCommand::Dial(peer));
            }
            Ok(_) => println!("Error: use a concrete IP and a nonzero port."),
            Err(error) if !self.registered => {
                println!("Error: invalid IP:port: {error}. Register to call a number.");
            }
            // Not an address: a number or user on the SIP server.
            Err(_) => {
                self.set_state(State::Calling);
                println!("Calling {text}");
                self.handle.send(UiCommand::DialTarget(text.to_owned()));
            }
        }
    }

    fn begin_quit(&mut self) {
        if self.quitting {
            // Second quit: stop waiting for the calls to finish.
            self.states = vec![State::Idle; LINES];
            return;
        }
        self.quitting = true;
        for line in 0..LINES {
            match self.states[line] {
                State::Idle => {}
                State::Incoming => {
                    println!("Rejecting the call, then exiting.");
                    self.handle.send(UiCommand::SelectLine(line));
                    self.handle.send(UiCommand::Reject);
                }
                _ => {
                    println!("Ending the call, then exiting.");
                    self.handle.send(UiCommand::SelectLine(line));
                    self.handle.send(UiCommand::Hangup);
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    let no_audio = args.iter().any(|argument| argument == "--no-audio");
    let trace = args.iter().any(|argument| argument == "--trace");
    let value = |name: &str| {
        args.iter()
            .find_map(|argument| argument.strip_prefix(name)?.strip_prefix('='))
    };
    let codecs = value("--codecs").map(Codec::parse_list);
    let encryption = value("--srtp").map(MediaEncryption::from_name);
    let transport = value("--transport").map_or(SipTransport::Udp, SipTransport::from_name);
    let proxy = value("--proxy").unwrap_or("").to_owned();
    let tls_verify = !args.iter().any(|argument| argument == "--tls-insecure");
    let tone_hz = match value("--tone").map(str::parse::<f32>) {
        Some(Ok(hz)) if (50.0..=3_400.0).contains(&hz) => Some(hz),
        Some(_) => return Err("--tone takes a pitch from 50 to 3400 Hz".into()),
        None => None,
    };
    let positional: Vec<&String> = args
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .collect();
    if positional.len() != 1 || args.len() - positional.len() > 9 {
        println!("{HELP}");
        return Err("expected LOCAL_IP:SIP_PORT".into());
    }

    let local: SocketAddr = positional[0].parse()?;
    if local.ip().is_unspecified() {
        return Err("use a concrete local IP, not 0.0.0.0".into());
    }

    let tone = tone_hz.map(|_| Arc::new(Mutex::new(ToneReport::default())));
    let handle = if let (Some(hz), Some(report)) = (tone_hz, tone.clone()) {
        let make = move || ToneMedia::new(hz, Arc::clone(&report));
        spawn_lines(local, "phone", Timers::default(), LINES, make)?
    } else if no_audio {
        spawn_lines(local, "phone", Timers::default(), LINES, NullMedia::default)?
    } else {
        spawn_lines(local, "phone", Timers::default(), LINES, AudioMedia::new)?
    };

    if trace {
        handle.send(UiCommand::Trace(true));
    }
    if let Some(codecs) = codecs {
        handle.send(UiCommand::Codecs(codecs));
    }
    if let Some(encryption) = encryption {
        handle.send(UiCommand::Encryption(encryption));
    }
    if let Some(stun) = value("--stun") {
        handle.send(UiCommand::Stun(Some(stun.to_owned())));
    }
    // With port 0 the system chose the port; report the real address.
    let local = handle.local_address();
    println!("Listening on {local}");
    println!("{COMMANDS}");
    if let Some(hz) = tone_hz {
        println!("Sending a {hz} Hz test tone instead of the microphone.");
    } else if no_audio {
        println!("Audio is disabled (--no-audio).");
    } else {
        println!("Use headphones. Incoming calls require answer.");
    }

    let (line_sender, lines) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if line_sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut phone = Phone {
        handle,
        local,
        states: vec![State::Idle; LINES],
        event_line: 0,
        selected: 0,
        registration: "not registered".to_owned(),
        registered: false,
        quitting: false,
        transport,
        proxy,
        tls_verify,
        tone,
    };
    let mut input_open = true;

    loop {
        while let Some(event) = phone.handle.try_event() {
            phone.on_event(event);
        }

        if input_open {
            match lines.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => phone.on_line(line.trim()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // Closed input behaves like quit, so a call is ended with BYE.
                    input_open = false;
                    phone.begin_quit();
                }
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }

        if phone.quitting && phone.states.iter().all(|state| *state == State::Idle) {
            break;
        }
    }

    // Dropping the handle stops the executor thread.
    drop(phone);
    Ok(())
}
