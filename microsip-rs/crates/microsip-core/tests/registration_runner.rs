//! Registration through the network executor against a scripted registrar
//! on a raw loopback socket. No sound devices are used.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{CallHandle, Event, Timers, UiCommand, spawn_with_timers};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::sip::digest::{DigestChallenge, authorization_header_with_cnonce};
use microsip_core::sip::message::{InviteRequest, Message, Request, ResponseBuilder};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const LOOPBACK: &str = "127.0.0.1:0";
const CHALLENGE: &str = "Digest realm=\"pbx\", nonce=\"nonce-1\", qop=\"auth\", algorithm=MD5";
const PASSWORD: &str = "secret";

fn fast_timers() -> Timers {
    Timers {
        t1: Duration::from_millis(30),
        t2: Duration::from_millis(100),
        give_up: Duration::from_millis(600),
    }
}

fn phone(timers: Timers) -> CallHandle {
    spawn_with_timers(
        LOOPBACK.parse().unwrap(),
        "phone",
        timers,
        NullMedia::default,
    )
    .expect("executor should start")
}

struct Registrar {
    socket: UdpSocket,
}

impl Registrar {
    fn new() -> Self {
        let socket = UdpSocket::bind(LOOPBACK).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        Self { socket }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn account(&self, password: &str) -> Account {
        Account::new(self.address().to_string(), "alice", password).with_expires(120)
    }

    fn receive(&self) -> (Request, SocketAddr) {
        let mut buffer = [0_u8; 65_535];
        loop {
            let (length, source) = self
                .socket
                .recv_from(&mut buffer)
                .expect("expected a request");
            let text = std::str::from_utf8(&buffer[..length]).unwrap();
            if let Message::Request(request) = Message::parse(text).unwrap() {
                return (request, source);
            }
        }
    }

    /// Next REGISTER that is not a retransmission of `previous`.
    fn next_register(&self, previous: Option<&Request>) -> (Request, SocketAddr) {
        loop {
            let (request, source) = self.receive();
            let repeated = previous
                .is_some_and(|earlier| earlier.headers.get("cseq") == request.headers.get("cseq"));
            if request.method.as_str() == "REGISTER" && !repeated {
                return (request, source);
            }
        }
    }

    /// Counts datagrams that arrive within `window`.
    fn count_for(&self, window: Duration) -> usize {
        let deadline = Instant::now() + window;
        let mut buffer = [0_u8; 65_535];
        let mut count = 0;
        self.socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        while Instant::now() < deadline {
            match self.socket.recv_from(&mut buffer) {
                Ok(_) => count += 1,
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(error) => panic!("registrar socket failed: {error}"),
            }
        }
        self.socket.set_read_timeout(Some(WAIT)).unwrap();
        count
    }

    fn reply(&self, request: &Request, peer: SocketAddr, status: u16, extra: &[(&str, &str)]) {
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
                format!("{};tag=registrar", request.headers.get("to").unwrap()),
            )
            .unwrap();
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
        }
        let bytes = Message::Response(builder.build().unwrap())
            .serialize()
            .unwrap();
        self.socket.send_to(&bytes, peer).unwrap();
    }

    /// Plays the usual exchange: challenge, checked credentials, 200.
    /// Returns the authorized REGISTER.
    fn accept(&self, expires: &str) -> (Request, SocketAddr) {
        let (first, peer) = self.next_register(None);
        assert!(first.headers.get("authorization").is_none());
        self.reply(&first, peer, 401, &[("www-authenticate", CHALLENGE)]);

        let (second, peer) = self.next_register(Some(&first));
        assert_digest_is_valid(&second, PASSWORD);
        self.reply(&second, peer, 200, &[("expires", expires)]);
        (second, peer)
    }
}

fn parameter(header: &str, name: &str) -> String {
    let start = header
        .find(&format!("{name}="))
        .unwrap_or_else(|| panic!("no {name} in {header}"))
        + name.len()
        + 1;
    header[start..]
        .split(',')
        .next()
        .unwrap()
        .trim()
        .trim_matches('"')
        .to_owned()
}

/// Recomputes the Digest response the way a server does.
fn assert_digest_is_valid(request: &Request, password: &str) {
    let sent = request
        .headers
        .get("authorization")
        .expect("credentials expected");
    let challenge = DigestChallenge::parse(CHALLENGE).unwrap();
    let expected = authorization_header_with_cnonce(
        &challenge,
        &parameter(sent, "username"),
        password,
        "REGISTER",
        &request.target,
        &parameter(sent, "cnonce"),
    )
    .unwrap();
    assert_eq!(parameter(sent, "username"), "alice");
    assert_eq!(
        parameter(sent, " response"),
        parameter(&expected, " response")
    );
}

fn wait_registration(handle: &CallHandle) -> RegistrationState {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match handle.wait_event(Duration::from_millis(50)) {
            Some(Event::Registration(state)) => return state,
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    panic!("timed out waiting for a registration event");
}

fn register(handle: &CallHandle, registrar: &Registrar) -> Request {
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));
    let (request, _) = registrar.accept("120");
    assert_eq!(wait_registration(handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(handle),
        RegistrationState::Registered { expires: 120 }
    );
    request
}

#[test]
fn registers_with_valid_digest_credentials() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());

    let request = register(&handle, &registrar);
    let local = handle.local_address();
    assert_eq!(request.target, format!("sip:{}", registrar.address().ip()));
    assert_eq!(request.headers.get("cseq"), Some("2 REGISTER"));
    assert_eq!(request.headers.get("expires"), Some("120"));
    assert_eq!(
        request.headers.get("contact"),
        Some(format!("<sip:alice@{local}>").as_str())
    );
    assert!(
        request
            .headers
            .get("via")
            .unwrap()
            .contains(&local.to_string())
    );
}

#[test]
fn dropping_the_handle_removes_the_registration() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    let registered = register(&handle, &registrar);

    let started = Instant::now();
    let dropper = thread::spawn(move || drop(handle));
    let (removal, peer) = registrar.next_register(Some(&registered));
    assert_eq!(removal.headers.get("expires"), Some("0"));
    assert_eq!(
        removal.headers.get("call-id"),
        registered.headers.get("call-id")
    );
    registrar.reply(&removal, peer, 200, &[]);
    dropper.join().unwrap();
    // The answer ends the shutdown at once, well inside the grace period.
    assert!(started.elapsed() < Duration::from_millis(1500));
}

#[test]
fn unregister_command_removes_the_registration_and_allows_a_new_one() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    let registered = register(&handle, &registrar);

    assert!(handle.send(UiCommand::Unregister));
    let (removal, peer) = registrar.next_register(Some(&registered));
    assert_eq!(removal.headers.get("expires"), Some("0"));
    registrar.reply(&removal, peer, 200, &[]);
    assert_eq!(wait_registration(&handle), RegistrationState::Unregistered);

    let again = register(&handle, &registrar);
    assert_ne!(
        again.headers.get("call-id"),
        registered.headers.get("call-id")
    );
}

#[test]
fn unregister_without_a_registration_reports_unregistered() {
    let handle = phone(Timers::default());
    assert!(handle.send(UiCommand::Unregister));
    assert_eq!(wait_registration(&handle), RegistrationState::Unregistered);
}

#[test]
fn wrong_password_fails_and_is_not_retried() {
    let registrar = Registrar::new();
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Register(registrar.account("wrong"))));

    let (first, peer) = registrar.next_register(None);
    registrar.reply(&first, peer, 401, &[("www-authenticate", CHALLENGE)]);
    let (second, peer) = registrar.next_register(Some(&first));
    assert!(second.headers.get("authorization").is_some());
    registrar.reply(&second, peer, 401, &[("www-authenticate", CHALLENGE)]);

    assert_eq!(wait_registration(&handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Failed("wrong user name or password (401)".to_owned())
    );
    assert_eq!(registrar.count_for(Duration::from_millis(300)), 0);
}

#[test]
fn registration_is_refreshed_before_it_expires() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));

    // One second granted: the refresh is due after half a second.
    let granted = Instant::now();
    let (registered, _) = registrar.accept("1");
    assert_eq!(wait_registration(&handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Registered { expires: 1 }
    );

    let (refresh, peer) = registrar.next_register(Some(&registered));
    let waited = granted.elapsed();
    assert!(
        waited >= Duration::from_millis(400),
        "too early: {waited:?}"
    );
    assert!(waited < Duration::from_millis(1000), "too late: {waited:?}");
    assert_eq!(
        refresh.headers.get("call-id"),
        registered.headers.get("call-id")
    );
    assert_eq!(refresh.headers.get("cseq"), Some("3 REGISTER"));
    assert_eq!(refresh.headers.get("expires"), Some("120"));
    registrar.reply(&refresh, peer, 200, &[("expires", "120")]);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Registered { expires: 120 }
    );
}

#[test]
fn silent_server_gets_retransmissions_and_then_a_failure() {
    let registrar = Registrar::new();
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));

    let (first, _) = registrar.next_register(None);
    // 30 ms doubling up to 100 ms for 600 ms: several identical copies.
    let mut copies = 1;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let (request, _) = registrar.receive();
        assert_eq!(request.headers.get("cseq"), first.headers.get("cseq"));
        assert_eq!(request.headers.get("via"), first.headers.get("via"));
        copies += 1;
        if copies >= 4 {
            break;
        }
    }
    assert!(copies >= 4, "only {copies} copies arrived");

    assert_eq!(wait_registration(&handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Failed("no response from the SIP server".to_owned())
    );
}

#[test]
fn late_answer_to_a_retransmission_still_registers() {
    let registrar = Registrar::new();
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));

    // Ignore the first copy, answer the second one.
    let (first, _) = registrar.receive();
    let (copy, peer) = registrar.receive();
    assert_eq!(copy.headers.get("via"), first.headers.get("via"));
    registrar.reply(&copy, peer, 200, &[("expires", "120")]);

    assert_eq!(wait_registration(&handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Registered { expires: 120 }
    );
    // Answered: the retransmissions stop.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(registrar.count_for(Duration::from_millis(300)), 0);
}

#[test]
fn invalid_account_fails_without_sending_anything() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());

    let mut account = registrar.account(PASSWORD);
    account.username = "al ice".to_owned();
    assert!(handle.send(UiCommand::Register(account)));
    assert!(matches!(
        wait_registration(&handle),
        RegistrationState::Failed(_)
    ));

    assert!(handle.send(UiCommand::Register(Account::new("", "alice", PASSWORD))));
    assert!(matches!(
        wait_registration(&handle),
        RegistrationState::Failed(_)
    ));
    assert_eq!(registrar.count_for(Duration::from_millis(200)), 0);
}

#[test]
fn loopback_phone_refuses_a_server_on_another_machine_at_once() {
    let handle = phone(Timers::default());
    // 192.0.2.0/24 is reserved for documentation; nothing is sent to it.
    let account = Account::new("192.0.2.1:5060", "alice", PASSWORD);
    assert!(handle.send(UiCommand::Register(account)));
    match wait_registration(&handle) {
        RegistrationState::Failed(reason) => {
            assert!(reason.contains("cannot reach 192.0.2.1:5060"), "{reason}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn second_register_while_registered_is_refused() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    register(&handle, &registrar);

    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "no error was reported");
        if let Some(Event::Error(message)) = handle.wait_event(Duration::from_millis(50)) {
            assert_eq!(message, "already registered; unregister first");
            break;
        }
    }
    assert_eq!(registrar.count_for(Duration::from_millis(200)), 0);
}

#[test]
fn call_from_the_server_is_offered_with_the_account_contact() {
    let registrar = Registrar::new();
    let handle = phone(fast_timers());
    register(&handle, &registrar);
    let local = handle.local_address();
    let server = registrar.address();

    let sdp = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\n\
               t=0 0\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let invite = InviteRequest::new(
        format!("sip:alice@{local}"),
        format!("sip:bob@{server}"),
        format!("sip:alice@{server}"),
        format!("sip:bob@{server}"),
        server.to_string(),
        "call-from-server",
        "z9hG4bK-call-from-server",
        "bob-tag",
    )
    .body(sdp.as_bytes().to_vec())
    .build()
    .unwrap();
    registrar
        .socket
        .send_to(&invite.serialize().unwrap(), local)
        .unwrap();

    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the call was not offered");
        match handle.wait_event(Duration::from_millis(50)) {
            Some(Event::Incoming { peer }) => {
                assert_eq!(peer, server);
                break;
            }
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }

    // The 180 names the registered user, not the executor's default one.
    let mut buffer = [0_u8; 65_535];
    let ringing = loop {
        let (length, _) = registrar.socket.recv_from(&mut buffer).unwrap();
        let text = std::str::from_utf8(&buffer[..length]).unwrap();
        if let Message::Response(response) = Message::parse(text).unwrap()
            && response.status_code == 180
        {
            break response;
        }
    };
    assert_eq!(
        ringing.headers.get("contact"),
        Some(format!("<sip:alice@{local}>").as_str())
    );
    assert!(handle.send(UiCommand::Reject));
}

#[test]
fn address_reported_by_the_server_is_registered_as_contact() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));

    // Answers 200 with a top Via that says where the request came from.
    let accept_as_seen_at = |request: &Request, peer: SocketAddr, seen: &str| {
        let (ip, port) = seen.rsplit_once(':').unwrap();
        let via = request.headers.get("via").unwrap();
        assert!(via.ends_with(";rport"), "no rport in {via}");
        let via = via.replace(";rport", &format!(";rport={port};received={ip}"));
        let mut builder = ResponseBuilder::new(200, "OK").header("via", via).unwrap();
        for name in ["from", "to", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        let response = builder.header("expires", "120").unwrap().build().unwrap();
        let bytes = Message::Response(response).serialize().unwrap();
        registrar.socket.send_to(&bytes, peer).unwrap();
    };

    let (first, peer) = registrar.next_register(None);
    let local = handle.local_address();
    assert_eq!(
        first.headers.get("contact"),
        Some(format!("<sip:alice@{local}>").as_str())
    );
    accept_as_seen_at(&first, peer, "127.0.0.9:45000");

    // The phone registers again with the address the server saw.
    let (second, peer) = registrar.next_register(Some(&first));
    assert_eq!(
        second.headers.get("contact"),
        Some("<sip:alice@127.0.0.9:45000>")
    );
    assert_eq!(second.headers.get("call-id"), first.headers.get("call-id"));
    accept_as_seen_at(&second, peer, "127.0.0.9:45000");

    assert_eq!(wait_registration(&handle), RegistrationState::Registering);
    assert_eq!(
        wait_registration(&handle),
        RegistrationState::Registered { expires: 120 }
    );
    assert_eq!(registrar.count_for(Duration::from_millis(200)), 0);
}

#[test]
fn trace_reports_both_directions_without_the_digest_response() {
    let registrar = Registrar::new();
    let handle = phone(Timers::default());
    assert!(handle.send(UiCommand::Trace(true)));
    assert!(handle.send(UiCommand::Register(registrar.account(PASSWORD))));
    let (authorized, _) = registrar.accept("120");
    let secret = parameter(
        authorized.headers.get("authorization").unwrap(),
        " response",
    );

    let mut sent = Vec::new();
    let mut received = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "registration did not finish");
        match handle.wait_event(Duration::from_millis(50)) {
            Some(Event::Trace {
                outgoing,
                peer,
                text,
            }) => {
                assert_eq!(peer, registrar.address());
                assert!(!text.contains(&secret), "the Digest response leaked");
                if outgoing {
                    sent.push(text);
                } else {
                    received.push(text);
                }
            }
            Some(Event::Registration(RegistrationState::Registered { .. })) => break,
            _ => {}
        }
    }
    assert_eq!(sent.len(), 2);
    assert!(sent[0].starts_with("REGISTER sip:"));
    assert!(sent[1].contains("response=\"<hidden>\""));
    assert!(sent[1].contains("username=\"alice\""));
    assert_eq!(received.len(), 2);
    assert!(received[0].starts_with("SIP/2.0 401"));
    assert!(received[0].contains("nonce=\"nonce-1\""));
    assert!(received[1].starts_with("SIP/2.0 200"));
}
