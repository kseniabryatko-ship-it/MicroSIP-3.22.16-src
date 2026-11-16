//! The phone with its account on TCP, against a scripted server on a
//! loopback connection: registration, calls in both directions, and a
//! connection that breaks. No sound devices are used.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use microsip_core::stream::{Framer, SipTransport};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(8);

fn phone() -> CallHandle {
    spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        Timers {
            t1: Duration::from_millis(30),
            t2: Duration::from_millis(100),
            give_up: Duration::from_millis(2_000),
        },
        NullMedia::default,
    )
    .expect("executor should start")
}

/// One accepted connection of the scripted server.
struct Connection {
    stream: TcpStream,
    framer: Framer,
    waiting: VecDeque<Vec<u8>>,
    /// The phone's end of the connection.
    peer: SocketAddr,
}

impl Connection {
    fn accept(listener: &TcpListener) -> Self {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            match listener.accept() {
                Ok((stream, peer)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    return Self {
                        stream,
                        framer: Framer::default(),
                        waiting: VecDeque::new(),
                        peer,
                    };
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the phone did not connect");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
    }

    /// The next message within `window`, if any.
    fn message_within(&mut self, window: Duration) -> Option<Message> {
        let deadline = Instant::now() + window;
        let mut buffer = [0_u8; 8_192];
        loop {
            if let Some(bytes) = self.waiting.pop_front() {
                let text = std::str::from_utf8(&bytes).unwrap();
                return Some(Message::parse(text).unwrap());
            }
            if Instant::now() >= deadline {
                return None;
            }
            match self.stream.read(&mut buffer) {
                Ok(0) => panic!("the phone closed the connection"),
                Ok(length) => self
                    .waiting
                    .extend(self.framer.push(&buffer[..length]).unwrap()),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => panic!("read failed: {error}"),
            }
        }
    }

    fn request(&mut self, method: &str) -> Request {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(Message::Request(request)) = self.message_within(WAIT)
                && request.method.as_str() == method
            {
                return request;
            }
        }
        panic!("no {method} arrived");
    }

    fn response(&mut self, status: u16) -> Response {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(Message::Response(response)) = self.message_within(WAIT)
                && response.status_code == status
            {
                return response;
            }
        }
        panic!("no {status} arrived");
    }

    fn reply(&mut self, request: &Request, status: u16, extra: &[(&str, &str)]) {
        let mut builder = ResponseBuilder::new(status, "Scripted");
        for via in request.headers.get_all("via") {
            builder = builder.append_header("via", via).unwrap();
        }
        for name in ["from", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        builder = builder
            .header(
                "to",
                format!("{};tag=server", request.headers.get("to").unwrap()),
            )
            .unwrap();
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
        }
        let bytes = Message::Response(builder.build().unwrap())
            .serialize()
            .unwrap();
        self.stream.write_all(&bytes).unwrap();
    }

    /// Accepts the registration without asking for a password.
    fn register(&mut self) -> Request {
        let request = self.request("REGISTER");
        self.reply(&request, 200, &[("expires", "120")]);
        request
    }
}

fn wait_for(handle: &CallHandle, wanted: impl Fn(&Event) -> bool, what: &str) -> Event {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match handle.wait_event(Duration::from_millis(50)) {
            Some(event) if wanted(&event) => return event,
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    panic!("timed out waiting for {what}");
}

fn registered(event: &Event) -> bool {
    matches!(
        event,
        Event::Registration(RegistrationState::Registered { .. })
    )
}

fn account(server: SocketAddr) -> Account {
    Account::new(server.to_string(), "alice", "secret")
        .with_expires(120)
        .with_transport(SipTransport::Tcp)
}

#[test]
fn account_on_tcp_registers_and_calls_through_the_one_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.local_addr().unwrap();
    let handle = phone();
    assert!(handle.send(UiCommand::Register(account(server))));
    let mut connection = Connection::accept(&listener);

    // The phone is known to the server by its end of the connection.
    let register = connection.register();
    let via = register.headers.get("via").unwrap();
    assert!(via.starts_with("SIP/2.0/TCP "), "{via}");
    assert_eq!(
        register.headers.get("contact"),
        Some(format!("<sip:alice@{};transport=tcp>", connection.peer).as_str())
    );
    wait_for(&handle, registered, "registration");

    // A call from the server comes in over the connection and is answered
    // over it.
    let sdp = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
               m=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let invite = InviteRequest::new(
        format!("sip:alice@{}", connection.peer),
        format!("sip:bob@{server}"),
        format!("sip:alice@{}", connection.peer),
        format!("sip:bob@{server}"),
        server.to_string(),
        "tcp-incoming",
        "z9hG4bK-tcp-incoming",
        "bob-tag",
    )
    .body(sdp.as_bytes().to_vec())
    .build()
    .unwrap();
    let bytes = invite.serialize().unwrap();
    // In two pieces, as a network may deliver it.
    connection.stream.write_all(&bytes[..40]).unwrap();
    connection.stream.flush().unwrap();
    std::thread::sleep(Duration::from_millis(40));
    connection.stream.write_all(&bytes[40..]).unwrap();
    connection.response(180);
    wait_for(
        &handle,
        |event| matches!(event, Event::Incoming { peer } if *peer == server),
        "the incoming call",
    );
    assert!(handle.send(UiCommand::Reject));
    let busy = connection.response(486);
    assert_eq!(busy.headers.get("call-id"), Some("tcp-incoming"));
    wait_for(
        &handle,
        |event| *event == Event::Ended(EndReason::Rejected(486)),
        "the end of the call",
    );

    // A call to a number goes out over the connection, once: what a
    // connection carries is not repeated.
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let invite = connection.request("INVITE");
    let via = invite.headers.get("via").unwrap();
    assert!(via.starts_with("SIP/2.0/TCP "), "{via}");
    let contact = invite.headers.get("contact").unwrap();
    assert!(contact.contains(";transport=tcp"), "{contact}");
    assert!(
        connection
            .message_within(Duration::from_millis(500))
            .is_none(),
        "the INVITE was sent again"
    );
    connection.reply(&invite, 486, &[]);
    let ack = connection.request("ACK");
    assert!(ack.headers.get("via").unwrap().starts_with("SIP/2.0/TCP "));
    wait_for(
        &handle,
        |event| *event == Event::Ended(EndReason::Rejected(486)),
        "the end of the call",
    );

    // Closing the phone takes the registration off over the connection.
    drop(handle);
    let removal = connection.request("REGISTER");
    assert_eq!(removal.headers.get("expires"), Some("0"));
}

#[test]
fn a_broken_connection_is_made_again_and_the_account_registered() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.local_addr().unwrap();
    let handle = phone();
    assert!(handle.send(UiCommand::Register(account(server))));
    let mut connection = Connection::accept(&listener);
    connection.register();
    wait_for(&handle, registered, "registration");
    let first_end = connection.peer;

    // The server goes away.
    drop(connection);
    let lost = wait_for(
        &handle,
        |event| matches!(event, Event::Registration(RegistrationState::Failed(_))),
        "the loss to be reported",
    );
    let Event::Registration(RegistrationState::Failed(reason)) = lost else {
        unreachable!();
    };
    assert!(reason.contains("connection"), "{reason}");

    // The phone comes back by itself, over a new connection.
    let mut connection = Connection::accept(&listener);
    assert_ne!(connection.peer, first_end);
    let register = connection.register();
    assert_eq!(
        register.headers.get("contact"),
        Some(format!("<sip:alice@{};transport=tcp>", connection.peer).as_str())
    );
    wait_for(&handle, registered, "the new registration");
}

#[test]
fn a_server_that_is_not_there_is_reported_and_tried_again() {
    // A port nobody listens on.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.local_addr().unwrap();
    drop(listener);
    let handle = phone();
    assert!(handle.send(UiCommand::Register(account(server))));
    let failed = wait_for(
        &handle,
        |event| matches!(event, Event::Registration(RegistrationState::Failed(_))),
        "the failure",
    );
    let Event::Registration(RegistrationState::Failed(reason)) = failed else {
        unreachable!();
    };
    assert!(reason.contains("over TCP"), "{reason}");

    // The server appears: the next try, a few seconds later, finds it.
    let listener = TcpListener::bind(server).unwrap();
    let mut connection = Connection::accept(&listener);
    connection.register();
    wait_for(&handle, registered, "registration");
}
