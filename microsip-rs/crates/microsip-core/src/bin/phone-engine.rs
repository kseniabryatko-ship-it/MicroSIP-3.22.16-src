//! Interactive direct-UDP phone on top of the call engine and network runner.
//!
//! One program handles both incoming and outgoing calls. It prints one status
//! line per event so scripts and, later, the GUI can follow it.

use microsip_core::call_media::{AudioMedia, NullMedia};
use microsip_core::call_runner::{CallHandle, EndReason, Event, UiCommand, spawn};
use microsip_core::registration::{Account, RegistrationState};
use std::error::Error;
use std::io::{self, BufRead};
use std::net::SocketAddr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

const HELP: &str = "Usage: phone-engine.exe LOCAL_IP:SIP_PORT [--no-audio] [--trace]\n\
Port 0 picks a free port; the chosen address is printed on the first line.\n\
Commands: call IP:PORT | call NUMBER | answer | reject | hangup | status | quit\n\
          register SERVER[:PORT] USER PASSWORD [DOMAIN] | unregister\n\
call IP:PORT dials directly. register keeps the account registered on a SIP\n\
server; after that the server can send calls here and call NUMBER (or\n\
USER@DOMAIN) dials through it.\n\
--no-audio runs without sound devices. --trace prints every SIP message\n\
(the Digest response value is hidden).";

const COMMANDS: &str = "Commands: call IP:PORT | call NUMBER | answer | reject | hangup | status | quit | \
register SERVER[:PORT] USER PASSWORD [DOMAIN] | unregister";

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
    state: State,
    registration: String,
    registered: bool,
    quitting: bool,
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

impl Phone {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Incoming { peer } => {
                self.state = State::Incoming;
                println!("Incoming call from {peer}. Type answer or reject.");
            }
            Event::Ringing => println!("Remote is ringing."),
            Event::Established => {
                self.state = State::Connected;
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
            Event::Ended(reason) => {
                self.state = State::Idle;
                println!("Call ended: {}", describe(reason));
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
        let mut parts = line.split_whitespace();
        match parts.next() {
            None => {}
            Some("call") => self.call(parts.next()),
            Some("answer") => {
                if self.state == State::Incoming {
                    self.state = State::Answering;
                    self.handle.send(UiCommand::Answer);
                } else {
                    println!("No incoming call to answer.");
                }
            }
            Some("reject") => {
                if self.state == State::Incoming {
                    self.handle.send(UiCommand::Reject);
                } else {
                    println!("No ringing call to reject.");
                }
            }
            Some("hangup") => {
                if self.state == State::Idle {
                    println!("No active call.");
                } else {
                    self.handle.send(UiCommand::Hangup);
                }
            }
            Some("status") => println!(
                "State: {}; local: {}; registration: {}",
                self.state.name(),
                self.local,
                self.registration
            ),
            Some("register") => {
                let (server, user, password) = (parts.next(), parts.next(), parts.next());
                let (Some(server), Some(user), Some(password)) = (server, user, password) else {
                    println!("Usage: register SERVER[:PORT] USER PASSWORD [DOMAIN]");
                    return;
                };
                let account =
                    Account::new(server, user, password).with_domain(parts.next().unwrap_or(""));
                self.handle.send(UiCommand::Register(account));
            }
            Some("unregister") => {
                self.handle.send(UiCommand::Unregister);
            }
            Some("quit") => self.begin_quit(),
            Some(_) => println!("{COMMANDS}"),
        }
    }

    fn call(&mut self, argument: Option<&str>) {
        if self.state != State::Idle {
            println!("A call is already in progress.");
            return;
        }
        let Some(text) = argument else {
            println!("Usage: call IP:PORT or call NUMBER");
            return;
        };
        match text.parse::<SocketAddr>() {
            Ok(peer) if !peer.ip().is_unspecified() && peer.port() != 0 => {
                self.state = State::Calling;
                println!("Calling {peer}");
                self.handle.send(UiCommand::Dial(peer));
            }
            Ok(_) => println!("Error: use a concrete IP and a nonzero port."),
            Err(error) if !self.registered => {
                println!("Error: invalid IP:port: {error}. Register to call a number.");
            }
            // Not an address: a number or user on the SIP server.
            Err(_) => {
                self.state = State::Calling;
                println!("Calling {text}");
                self.handle.send(UiCommand::DialTarget(text.to_owned()));
            }
        }
    }

    fn begin_quit(&mut self) {
        if self.quitting {
            // Second quit: stop waiting for the call to finish.
            self.state = State::Idle;
            return;
        }
        self.quitting = true;
        match self.state {
            State::Idle => {}
            State::Incoming => {
                println!("Rejecting the call, then exiting.");
                self.handle.send(UiCommand::Reject);
            }
            _ => {
                println!("Ending the call, then exiting.");
                self.handle.send(UiCommand::Hangup);
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
    let positional: Vec<&String> = args
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .collect();
    if positional.len() != 1 || args.len() - positional.len() > 2 {
        println!("{HELP}");
        return Err("expected LOCAL_IP:SIP_PORT".into());
    }

    let local: SocketAddr = positional[0].parse()?;
    if local.ip().is_unspecified() {
        return Err("use a concrete local IP, not 0.0.0.0".into());
    }

    let handle = if no_audio {
        spawn(local, "phone", NullMedia::default)?
    } else {
        spawn(local, "phone", AudioMedia::new)?
    };

    if trace {
        handle.send(UiCommand::Trace(true));
    }
    // With port 0 the system chose the port; report the real address.
    let local = handle.local_address();
    println!("Listening on {local}");
    println!("{COMMANDS}");
    if no_audio {
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
        state: State::Idle,
        registration: "not registered".to_owned(),
        registered: false,
        quitting: false,
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

        if phone.quitting && phone.state == State::Idle {
            break;
        }
    }

    // Dropping the handle stops the executor thread.
    drop(phone);
    Ok(())
}
