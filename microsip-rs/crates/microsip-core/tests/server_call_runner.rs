//! Calls placed and received through a SIP server, against a scripted server
//! on a raw loopback socket. No sound devices are used.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, TransferState, UiCommand, spawn_with_timers,
};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::sip::digest::{DigestChallenge, authorization_header_with_cnonce};
use microsip_core::sip::message::{
    InviteRequest, Message, Method, Request, RequestBuilder, Response, ResponseBuilder,
};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const LOOPBACK: &str = "127.0.0.1:0";
const CHALLENGE: &str = "Digest realm=\"pbx\", nonce=\"call-nonce\", qop=\"auth\", algorithm=MD5";
const PASSWORD: &str = "secret";
const SDP: &str = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\n\
                   t=0 0\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";

fn fast_timers() -> Timers {
    Timers {
        t1: Duration::from_millis(30),
        t2: Duration::from_millis(100),
        give_up: Duration::from_millis(600),
    }
}

/// Slow enough that no retransmission disturbs a scripted exchange.
fn calm_timers() -> Timers {
    Timers {
        t1: Duration::from_secs(2),
        t2: Duration::from_secs(4),
        give_up: Duration::from_secs(8),
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

struct Server {
    socket: UdpSocket,
}

impl Server {
    fn new() -> Self {
        let socket = UdpSocket::bind(LOOPBACK).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        Self { socket }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn receive(&self) -> (Message, SocketAddr) {
        let mut buffer = [0_u8; 65_535];
        let (length, source) = self
            .socket
            .recv_from(&mut buffer)
            .expect("expected a datagram");
        let text = std::str::from_utf8(&buffer[..length]).unwrap();
        (Message::parse(text).unwrap(), source)
    }

    /// Next request with this method; other datagrams are skipped.
    fn request(&self, method: &str) -> (Request, SocketAddr) {
        loop {
            if let (Message::Request(request), source) = self.receive()
                && request.method.as_str() == method
            {
                return (request, source);
            }
        }
    }

    /// Next response with this status; other datagrams are skipped.
    fn response(&self, status: u16) -> Response {
        loop {
            if let (Message::Response(response), _) = self.receive()
                && response.status_code == status
            {
                return response;
            }
        }
    }

    /// Methods of the requests that arrive within `window`.
    fn methods_for(&self, window: Duration) -> Vec<String> {
        let deadline = Instant::now() + window;
        let mut buffer = [0_u8; 65_535];
        let mut methods = Vec::new();
        self.socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        while Instant::now() < deadline {
            match self.socket.recv_from(&mut buffer) {
                Ok((length, _)) => {
                    let text = std::str::from_utf8(&buffer[..length]).unwrap();
                    if let Message::Request(request) = Message::parse(text).unwrap() {
                        methods.push(request.method.as_str().to_owned());
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(error) => panic!("server socket failed: {error}"),
            }
        }
        self.socket.set_read_timeout(Some(WAIT)).unwrap();
        methods
    }

    fn reply(
        &self,
        request: &Request,
        peer: SocketAddr,
        status: u16,
        extra: &[(&str, &str)],
        body: Option<&str>,
    ) {
        let mut builder = ResponseBuilder::new(status, "Scripted");
        for via in request.headers.get_all("via") {
            builder = builder.append_header("via", via).unwrap();
        }
        for name in ["from", "call-id", "cseq"] {
            builder = builder
                .header(name, request.headers.get(name).unwrap())
                .unwrap();
        }
        let to = request.headers.get("to").unwrap();
        let to = if to.contains(";tag=") || status == 100 {
            to.to_owned()
        } else {
            format!("{to};tag=server-tag")
        };
        builder = builder.header("to", to).unwrap();
        for (name, value) in extra {
            builder = builder.append_header(name, *value).unwrap();
        }
        if let Some(body) = body {
            builder = builder
                .header("content-type", "application/sdp")
                .unwrap()
                .body(body.as_bytes().to_vec());
        }
        let bytes = Message::Response(builder.build().unwrap())
            .serialize()
            .unwrap();
        self.socket.send_to(&bytes, peer).unwrap();
    }

    fn send(&self, request: &Request, peer: SocketAddr) {
        self.socket
            .send_to(&request.serialize().unwrap(), peer)
            .unwrap();
    }

    /// Accepts the registration without asking for a password.
    fn register(&self, handle: &CallHandle, password: &str) {
        let account = Account::new(self.address().to_string(), "alice", password);
        assert!(handle.send(UiCommand::Register(account)));
        let (request, peer) = self.request("REGISTER");
        self.reply(&request, peer, 200, &[("expires", "3600")], None);
        wait_for(
            handle,
            Event::Registration(RegistrationState::Registered { expires: 3600 }),
        );
    }
}

fn wait_for(handle: &CallHandle, expected: Event) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match handle.wait_event(Duration::from_millis(50)) {
            Some(event) if event == expected => return,
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    panic!("timed out waiting for {expected:?}");
}

fn wait_error(handle: &CallHandle) -> String {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(Event::Error(message)) = handle.wait_event(Duration::from_millis(50)) {
            return message;
        }
    }
    panic!("timed out waiting for an error");
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

/// Recomputes the Digest response of an INVITE the way a server does.
fn assert_invite_digest_is_valid(request: &Request) {
    let sent = request
        .headers
        .get("proxy-authorization")
        .expect("credentials expected");
    let challenge = DigestChallenge::parse(CHALLENGE).unwrap();
    let expected = authorization_header_with_cnonce(
        &challenge,
        "alice",
        PASSWORD,
        "INVITE",
        &request.target,
        &parameter(sent, "cnonce"),
    )
    .unwrap();
    assert_eq!(parameter(sent, "username"), "alice");
    assert_eq!(parameter(sent, " uri"), request.target);
    assert_eq!(
        parameter(sent, " response"),
        parameter(&expected, " response")
    );
}

fn branch(request: &Request) -> String {
    parameter(request.headers.get("via").unwrap(), "branch")
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Dials `100` and plays the server side up to the authorized INVITE.
fn dial_with_challenge(
    server: &Server,
    handle: &CallHandle,
) -> (Request, Request, Request, SocketAddr) {
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let (first, peer) = server.request("INVITE");
    assert!(first.headers.get("proxy-authorization").is_none());
    server.reply(
        &first,
        peer,
        407,
        &[("proxy-authenticate", CHALLENGE)],
        None,
    );

    let (ack, _) = server.request("ACK");
    let (second, _) = server.request("INVITE");
    (first, ack, second, peer)
}

#[test]
fn number_is_called_through_the_server_with_credentials_and_routes() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, PASSWORD);
    let local = handle.local_address();
    let domain = server.address().ip();

    let (first, ack, second, peer) = dial_with_challenge(&server, &handle);

    // The first INVITE names the number on the account's domain.
    assert_eq!(first.target, format!("sip:100@{domain}"));
    assert_eq!(first.headers.get("cseq"), Some("1 INVITE"));
    assert!(
        first
            .headers
            .get("from")
            .unwrap()
            .starts_with(&format!("<sip:alice@{domain}>;tag="))
    );
    assert_eq!(
        first.headers.get("to"),
        Some(format!("<sip:100@{domain}>").as_str())
    );
    assert_eq!(
        first.headers.get("contact"),
        Some(format!("<sip:alice@{local}>").as_str())
    );

    // The challenge is acknowledged inside the first transaction.
    assert_eq!(ack.headers.get("cseq"), Some("1 ACK"));
    assert_eq!(branch(&ack), branch(&first));

    // The second INVITE continues the same call with valid credentials.
    assert_eq!(second.target, first.target);
    assert_eq!(second.headers.get("cseq"), Some("2 INVITE"));
    assert_eq!(second.headers.get("call-id"), first.headers.get("call-id"));
    assert_eq!(second.headers.get("from"), first.headers.get("from"));
    assert_ne!(branch(&second), branch(&first));
    assert_eq!(second.body, first.body);
    assert_invite_digest_is_valid(&second);

    server.reply(&second, peer, 180, &[], None);
    wait_for(&handle, Event::Ringing);

    let contact = format!("<sip:100@{}>", server.address());
    server.reply(
        &second,
        peer,
        200,
        &[
            ("contact", contact.as_str()),
            ("record-route", "<sip:p1.test;lr>, <sip:p2.test;lr>"),
        ],
        Some(SDP),
    );
    wait_for(&handle, Event::Established);

    // The caller walks the recorded route backwards.
    let reversed = ["<sip:p2.test;lr>", "<sip:p1.test;lr>"];
    let (ack, _) = server.request("ACK");
    assert_eq!(ack.target, format!("sip:100@{}", server.address()));
    assert_eq!(ack.headers.get("cseq"), Some("2 ACK"));
    assert_eq!(ack.headers.get_all("route"), reversed);

    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("3 BYE"));
    assert_eq!(bye.headers.get_all("route"), reversed);
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn second_challenge_ends_the_call_as_rejected() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, "wrong");

    let (_, _, second, peer) = dial_with_challenge(&server, &handle);
    assert!(second.headers.get("proxy-authorization").is_some());
    server.reply(
        &second,
        peer,
        407,
        &[("proxy-authenticate", CHALLENGE)],
        None,
    );

    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("2 ACK"));
    assert_eq!(branch(&ack), branch(&second));
    wait_for(&handle, Event::Ended(EndReason::Rejected(407)));
    assert!(
        !server
            .methods_for(Duration::from_millis(300))
            .contains(&"INVITE".to_owned())
    );
}

#[test]
fn repeated_challenge_for_the_replaced_invite_is_acknowledged_again() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, PASSWORD);

    let (first, first_ack, second, peer) = dial_with_challenge(&server, &handle);

    // The server did not get our ACK and repeats its 407.
    server.reply(
        &first,
        peer,
        407,
        &[("proxy-authenticate", CHALLENGE)],
        None,
    );
    let (repeated, _) = server.request("ACK");
    assert_eq!(repeated.headers.get("cseq"), Some("1 ACK"));
    assert_eq!(branch(&repeated), branch(&first_ack));

    // The authorized INVITE is still the live one.
    server.reply(&second, peer, 486, &[], None);
    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("2 ACK"));
    wait_for(&handle, Event::Ended(EndReason::Rejected(486)));
}

#[test]
fn challenge_after_hangup_is_not_answered_with_a_new_invite() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, PASSWORD);

    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let (first, peer) = server.request("INVITE");
    assert!(handle.send(UiCommand::Hangup));
    std::thread::sleep(Duration::from_millis(150));
    server.reply(
        &first,
        peer,
        407,
        &[("proxy-authenticate", CHALLENGE)],
        None,
    );

    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("1 ACK"));
    wait_for(&handle, Event::Ended(EndReason::Cancelled));
    assert!(
        !server
            .methods_for(Duration::from_millis(300))
            .contains(&"INVITE".to_owned())
    );
}

#[test]
fn full_address_keeps_its_own_domain_but_goes_to_the_server() {
    let server = Server::new();
    let handle = phone(fast_timers());
    server.register(&handle, PASSWORD);

    assert!(handle.send(UiCommand::DialTarget(" sip:200@other.example ".to_owned())));
    let (invite, peer) = server.request("INVITE");
    assert_eq!(invite.target, "sip:200@other.example");
    assert_eq!(invite.headers.get("to"), Some("<sip:200@other.example>"));
    server.reply(&invite, peer, 404, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Rejected(404)));
}

#[test]
fn number_without_registration_fails_without_sending_anything() {
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    assert_eq!(
        wait_error(&handle),
        "register on a SIP server before calling a number"
    );
    wait_for(&handle, Event::Ended(EndReason::Failed));
}

#[test]
fn malformed_number_fails_without_sending_anything() {
    let server = Server::new();
    let handle = phone(fast_timers());
    server.register(&handle, PASSWORD);

    for target in ["1 00", "@pbx", "100@", "a;b", ""] {
        assert!(handle.send(UiCommand::DialTarget(target.to_owned())));
        assert_eq!(
            wait_error(&handle),
            "invalid number or SIP address",
            "{target:?}"
        );
        wait_for(&handle, Event::Ended(EndReason::Failed));
    }
    assert!(
        !server
            .methods_for(Duration::from_millis(200))
            .contains(&"INVITE".to_owned())
    );
}

#[test]
fn address_text_is_still_called_directly() {
    let peer = Server::new();
    let handle = phone(fast_timers());

    assert!(handle.send(UiCommand::DialTarget(peer.address().to_string())));
    let (invite, source) = peer.request("INVITE");
    assert_eq!(invite.target, format!("sip:phone@{}", peer.address()));
    peer.reply(&invite, source, 486, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Rejected(486)));
}

#[test]
fn called_side_echoes_the_recorded_route_and_uses_it_for_bye() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, PASSWORD);
    let local = handle.local_address();
    let address = server.address();

    let mut invite = InviteRequest::new(
        format!("sip:alice@{local}"),
        format!("sip:bob@{address}"),
        format!("sip:alice@{address}"),
        format!("sip:bob@{address}"),
        address.to_string(),
        "routed-call",
        "z9hG4bK-routed-call",
        "bob-tag",
    )
    .body(SDP.as_bytes().to_vec())
    .build()
    .unwrap();
    let routes = ["<sip:p1.test;lr>", "<sip:p2.test;lr>"];
    for route in routes {
        invite.headers.append("record-route", route).unwrap();
    }
    server.send(&invite, local);
    wait_for(&handle, Event::Incoming { peer: address });

    let ringing = server.response(180);
    assert_eq!(ringing.headers.get_all("record-route"), routes);

    assert!(handle.send(UiCommand::Answer));
    let ok = server.response(200);
    assert_eq!(ok.headers.get_all("record-route"), routes);

    let mut dialog = EstablishedDialog::new(DialogConfig {
        remote_target: format!("sip:alice@{local}"),
        local_from: format!("<sip:bob@{address}>;tag=bob-tag"),
        remote_to: ok.headers.get("to").unwrap().to_owned(),
        call_id: "routed-call".to_owned(),
        local_sent_by: address.to_string(),
        local_contact: format!("<sip:bob@{address}>"),
        invite_cseq: 1,
        invite_branch: "z9hG4bK-routed-call".to_owned(),
    })
    .unwrap();
    server.send(
        &dialog.build_success_ack("z9hG4bK-routed-ack").unwrap(),
        local,
    );
    wait_for(&handle, Event::Established);

    // The called side keeps the route in the order it was received.
    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.target, format!("sip:bob@{address}"));
    assert_eq!(bye.headers.get_all("route"), routes);
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn offer_proposes_keypad_events_and_answer_follows_the_caller() {
    let server = Server::new();
    let handle = phone(fast_timers());
    let local = handle.local_address();
    let address = server.address();

    // Our own offer proposes both G.711 codecs and telephone-event.
    assert!(handle.send(UiCommand::DialTarget(address.to_string())));
    let (invite, peer) = server.request("INVITE");
    let offer = std::str::from_utf8(&invite.body).unwrap();
    assert!(offer.contains(" RTP/AVP 0 8 101\r\n"), "{offer}");
    assert!(offer.contains("a=rtpmap:8 PCMA/8000\r\n"), "{offer}");
    assert!(offer.contains("a=rtpmap:101 telephone-event/8000\r\n"));
    server.reply(&invite, peer, 486, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Rejected(486)));

    // A caller's offer decides what our answer contains.
    let answer_to = |call_id: &str, formats: &str, extra: &str| {
        let sdp = format!(
            "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
             m=audio 40000 RTP/AVP {formats}\r\na=rtpmap:0 PCMU/8000\r\n{extra}"
        );
        let invite = InviteRequest::new(
            format!("sip:phone@{local}"),
            format!("sip:bob@{address}"),
            format!("sip:phone@{local}"),
            format!("sip:bob@{address}"),
            address.to_string(),
            call_id,
            format!("z9hG4bK-{call_id}"),
            "bob-tag",
        )
        .body(sdp.into_bytes())
        .build()
        .unwrap();
        server.send(&invite, local);
        wait_for(&handle, Event::Incoming { peer: address });
        assert!(handle.send(UiCommand::Answer));
        // Skip repeated answers that are still arriving from an earlier call.
        let ok = loop {
            let response = server.response(200);
            if response.headers.get("call-id") == Some(call_id) {
                break response;
            }
        };
        // No ACK follows: with the fast timers the phone gives the call up.
        wait_for(&handle, Event::Ended(EndReason::Timeout));
        String::from_utf8(ok.body).unwrap()
    };

    let answer = answer_to(
        "with-events",
        "0 96",
        "a=rtpmap:96 telephone-event/8000\r\n",
    );
    assert!(answer.contains(" RTP/AVP 0 96\r\n"), "{answer}");
    assert!(answer.contains("a=rtpmap:96 telephone-event/8000\r\n"));

    let answer = answer_to("without-events", "0", "");
    assert!(answer.contains(" RTP/AVP 0\r\n"), "{answer}");
    assert!(!answer.contains("telephone-event"));
}

#[test]
fn hangup_with_no_call_is_quietly_ignored_but_a_digit_is_refused() {
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Hangup));
    assert!(handle.send(UiCommand::Mute(true)));
    assert!(handle.send(UiCommand::Dtmf('5')));
    // The only thing reported is the digit that could not be sent.
    assert_eq!(
        wait_error(&handle),
        "digits can be sent only during a connected call"
    );
    assert!(handle.wait_event(Duration::from_millis(200)).is_none());
}

/// Registers, dials `100` and answers it. Returns the INVITE and its source.
fn connect(server: &Server, handle: &CallHandle) -> (Request, SocketAddr) {
    server.register(handle, PASSWORD);
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let (invite, peer) = server.request("INVITE");
    let contact = format!("<sip:100@{}>", server.address());
    server.reply(
        &invite,
        peer,
        200,
        &[("contact", contact.as_str())],
        Some(SDP),
    );
    server.request("ACK");
    wait_for(handle, Event::Established);
    (invite, peer)
}

fn text(body: &[u8]) -> &str {
    std::str::from_utf8(body).unwrap()
}

#[test]
fn hold_and_resume_are_requested_inside_the_call() {
    let server = Server::new();
    let handle = phone(calm_timers());
    let (invite, _) = connect(&server, &handle);
    let recvonly = SDP.to_owned() + "a=recvonly\r\n";

    assert!(handle.send(UiCommand::Hold(true)));
    let (hold, peer) = server.request("INVITE");
    assert_eq!(hold.headers.get("call-id"), invite.headers.get("call-id"));
    assert_eq!(hold.headers.get("cseq"), Some("2 INVITE"));
    assert_eq!(hold.target, format!("sip:100@{}", server.address()));
    assert_eq!(hold.headers.get("from"), invite.headers.get("from"));
    assert!(hold.headers.get("to").unwrap().ends_with(";tag=server-tag"));
    assert_eq!(hold.headers.get("content-type"), Some("application/sdp"));
    assert!(hold.headers.get("contact").is_some());
    let offer = text(&hold.body);
    assert!(offer.contains("a=sendonly\r\n"), "{offer}");
    // The description changed, so its version is higher than the first one.
    assert!(text(&invite.body).contains("o=phone 1 1 "));
    assert!(offer.contains("o=phone 1 2 "), "{offer}");
    // Same audio port as before. The answer had no telephone-event, so the
    // new offer no longer lists it.
    let audio_port = |body: &str| {
        let line = body
            .lines()
            .find(|line| line.starts_with("m=audio"))
            .unwrap();
        line.split(' ').nth(1).unwrap().to_owned()
    };
    assert_eq!(audio_port(offer), audio_port(text(&invite.body)));
    assert!(offer.contains(" RTP/AVP 0\r\n"), "{offer}");

    server.reply(&hold, peer, 200, &[], Some(&recvonly));
    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("2 ACK"));
    assert_ne!(branch(&ack), branch(&hold));
    wait_for(&handle, Event::Hold(true));

    // A repeated answer gets the same ACK again and no second report.
    server.reply(&hold, peer, 200, &[], Some(&recvonly));
    let (again, _) = server.request("ACK");
    assert_eq!(again.headers.get("via"), ack.headers.get("via"));

    assert!(handle.send(UiCommand::Hold(false)));
    let (resume, peer) = server.request("INVITE");
    assert_eq!(resume.headers.get("cseq"), Some("3 INVITE"));
    let offer = text(&resume.body);
    assert!(offer.contains("a=sendrecv\r\n"), "{offer}");
    assert!(offer.contains("o=phone 1 3 "), "{offer}");
    server.reply(&resume, peer, 200, &[], Some(SDP));
    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("3 ACK"));
    wait_for(&handle, Event::Hold(false));

    // The call goes on with the next sequence number.
    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("4 BYE"));
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn refused_hold_is_acknowledged_and_the_call_stays_connected() {
    let server = Server::new();
    let handle = phone(calm_timers());
    connect(&server, &handle);

    assert!(handle.send(UiCommand::Hold(true)));
    let (hold, peer) = server.request("INVITE");
    server.reply(&hold, peer, 488, &[], None);
    let (ack, _) = server.request("ACK");
    assert_eq!(ack.headers.get("cseq"), Some("2 ACK"));
    assert_eq!(branch(&ack), branch(&hold));
    assert_eq!(wait_error(&handle), "Hold did not work: refused (488)");
    wait_for(&handle, Event::Hold(false));

    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("3 BYE"));
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn unanswered_hold_is_given_up_and_reported() {
    let server = Server::new();
    let handle = phone(fast_timers());
    connect(&server, &handle);

    assert!(handle.send(UiCommand::Hold(true)));
    let (hold, _) = server.request("INVITE");
    // The request is repeated while nobody answers.
    let (copy, _) = server.request("INVITE");
    assert_eq!(copy.headers.get("via"), hold.headers.get("via"));
    assert_eq!(wait_error(&handle), "Hold did not work: no answer");
    wait_for(&handle, Event::Hold(false));
}

#[test]
fn hold_without_a_connected_call_is_refused() {
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Hold(true)));
    assert_eq!(
        wait_error(&handle),
        "only a connected call can be put on hold"
    );
}

#[test]
fn hold_by_the_other_side_is_answered_and_reported() {
    let server = Server::new();
    let handle = phone(calm_timers());
    let (invite, _) = connect(&server, &handle);
    let local = handle.local_address();
    let address = server.address();
    let our_from = invite.headers.get("from").unwrap();
    let our_tag = our_from.rsplit_once("tag=").unwrap().1;

    // An INVITE from the other side of the same call.
    let reinvite = |cseq: u32, direction: &str| {
        let sdp = format!("{SDP}a={direction}\r\n");
        InviteRequest::new(
            format!("sip:alice@{local}"),
            format!("sip:100@{}", address.ip()),
            format!("sip:alice@{}", address.ip()),
            format!("sip:100@{address}"),
            address.to_string(),
            invite.headers.get("call-id").unwrap(),
            format!("z9hG4bK-reinvite-{cseq}"),
            "server-tag",
        )
        .to_tag(our_tag)
        .cseq(cseq)
        .body(sdp.into_bytes())
        .build()
        .unwrap()
    };

    let hold = reinvite(1, "sendonly");
    server.send(&hold, local);
    let ok = server.response(200);
    assert_eq!(ok.headers.get("cseq"), Some("1 INVITE"));
    assert!(ok.headers.get("contact").is_some());
    let answer = text(&ok.body);
    // They only send, so we only receive.
    assert!(answer.contains("a=recvonly\r\n"), "{answer}");
    assert!(answer.contains("o=phone 1 2 "), "{answer}");
    wait_for(&handle, Event::RemoteHold(true));

    // Our answer was lost and the request comes again: same answer, no
    // second change of the description.
    server.send(&hold, local);
    let again = server.response(200);
    assert_eq!(again.body, ok.body);

    server.send(&reinvite(2, "sendrecv"), local);
    let ok = server.response(200);
    let answer = text(&ok.body);
    assert!(answer.contains("a=sendrecv\r\n"), "{answer}");
    assert!(answer.contains("o=phone 1 3 "), "{answer}");
    wait_for(&handle, Event::RemoteHold(false));

    // The call itself was never disturbed.
    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn second_call_is_still_refused_as_busy() {
    let server = Server::new();
    let handle = phone(calm_timers());
    connect(&server, &handle);
    let local = handle.local_address();
    let address = server.address();

    let other = InviteRequest::new(
        format!("sip:alice@{local}"),
        format!("sip:carol@{address}"),
        format!("sip:alice@{address}"),
        format!("sip:carol@{address}"),
        address.to_string(),
        "another-call",
        "z9hG4bK-another-call",
        "carol-tag",
    )
    .body(SDP.as_bytes().to_vec())
    .build()
    .unwrap();
    server.send(&other, local);
    let busy = server.response(486);
    assert_eq!(busy.headers.get("call-id"), Some("another-call"));
}

#[test]
fn caller_offering_only_pcma_is_answered_with_pcma() {
    let server = Server::new();
    let handle = phone(fast_timers());
    let local = handle.local_address();
    let address = server.address();

    let sdp = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
               m=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    let invite = InviteRequest::new(
        format!("sip:phone@{local}"),
        format!("sip:bob@{address}"),
        format!("sip:phone@{local}"),
        format!("sip:bob@{address}"),
        address.to_string(),
        "pcma-only",
        "z9hG4bK-pcma-only",
        "bob-tag",
    )
    .body(sdp.as_bytes().to_vec())
    .build()
    .unwrap();
    server.send(&invite, local);
    wait_for(&handle, Event::Incoming { peer: address });
    assert!(handle.send(UiCommand::Answer));
    let ok = server.response(200);
    let answer = text(&ok.body);
    assert!(answer.contains(" RTP/AVP 8\r\n"), "{answer}");
    assert!(answer.contains("a=rtpmap:8 PCMA/8000\r\n"), "{answer}");
    assert!(!answer.contains("PCMU"), "{answer}");
}

#[test]
fn caller_offering_no_g711_is_refused_with_488() {
    let server = Server::new();
    let handle = phone(fast_timers());
    let local = handle.local_address();
    let address = server.address();

    let sdp = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
               m=audio 40000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
    let invite = InviteRequest::new(
        format!("sip:phone@{local}"),
        format!("sip:bob@{address}"),
        format!("sip:phone@{local}"),
        format!("sip:bob@{address}"),
        address.to_string(),
        "g729-only",
        "z9hG4bK-g729-only",
        "bob-tag",
    )
    .body(sdp.as_bytes().to_vec())
    .build()
    .unwrap();
    server.send(&invite, local);
    let refused = server.response(488);
    assert_eq!(refused.headers.get("call-id"), Some("g729-only"));
}

#[test]
fn answer_choosing_pcma_connects_the_call() {
    let server = Server::new();
    let handle = phone(calm_timers());
    server.register(&handle, PASSWORD);
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let (invite, peer) = server.request("INVITE");

    let pcma = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
                m=audio 40000 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\n";
    let contact = format!("<sip:100@{}>", server.address());
    server.reply(
        &invite,
        peer,
        200,
        &[("contact", contact.as_str())],
        Some(pcma),
    );
    server.request("ACK");
    wait_for(&handle, Event::Established);

    // A later hold offers the codec the call is using, not the whole list.
    assert!(handle.send(UiCommand::Hold(true)));
    let (hold, _) = server.request("INVITE");
    let offer = text(&hold.body);
    assert!(offer.contains(" RTP/AVP 8\r\n"), "{offer}");
}

#[test]
fn recording_needs_a_connected_call_and_stopping_is_always_allowed() {
    let handle = phone(fast_timers());
    let path = std::env::temp_dir().join("microsip-rs-never-written.wav");
    let _ = std::fs::remove_file(&path);

    assert!(handle.send(UiCommand::Record(Some(path.clone()))));
    assert_eq!(wait_error(&handle), "only a connected call can be recorded");
    assert!(!path.exists(), "no file may appear without a call");

    // Stopping when nothing is recorded is not an error.
    assert!(handle.send(UiCommand::Record(None)));
    wait_for(&handle, Event::Recording(None));
}

/// A request from the server inside the call started by `invite`.
fn in_dialog(
    server: &Server,
    invite: &Request,
    method: Method,
    cseq: u32,
    extra: &[(&str, &str)],
    body: &str,
) -> Request {
    let our_from = invite.headers.get("from").unwrap();
    let name = method.as_str().to_owned();
    let mut builder = RequestBuilder::new(
        method,
        invite
            .headers
            .get("contact")
            .unwrap()
            .trim_matches(['<', '>']),
    )
    .header(
        "via",
        format!(
            "SIP/2.0/UDP {};branch=z9hG4bK-{name}-{cseq}",
            server.address()
        ),
    )
    .unwrap()
    // The server's side of the dialog: our To with its tag is its From.
    .header(
        "from",
        format!("{};tag=server-tag", invite.headers.get("to").unwrap()),
    )
    .unwrap()
    .header("to", our_from)
    .unwrap()
    .header("call-id", invite.headers.get("call-id").unwrap())
    .unwrap()
    .header("cseq", format!("{cseq} {name}"))
    .unwrap()
    .header("max-forwards", "70")
    .unwrap();
    for (name, value) in extra {
        builder = builder.header(name, *value).unwrap();
    }
    builder.body(body.as_bytes().to_vec()).build().unwrap()
}

#[test]
fn transfer_refers_the_call_and_leaves_when_the_new_call_is_up() {
    let server = Server::new();
    let handle = phone(calm_timers());
    let (invite, _) = connect(&server, &handle);
    let local = handle.local_address();
    let domain = server.address().ip();

    assert!(handle.send(UiCommand::Transfer(" 4011 ".to_owned())));
    let (refer, peer) = server.request("REFER");
    assert_eq!(refer.headers.get("call-id"), invite.headers.get("call-id"));
    assert_eq!(refer.headers.get("cseq"), Some("2 REFER"));
    assert_eq!(refer.target, format!("sip:100@{}", server.address()));
    assert_eq!(
        refer.headers.get("refer-to"),
        Some(format!("<sip:4011@{domain}>").as_str())
    );
    assert_eq!(
        refer.headers.get("referred-by"),
        Some(format!("<sip:alice@{domain}>").as_str())
    );
    server.reply(&refer, peer, 202, &[], None);
    wait_for(&handle, Event::Transfer(TransferState::Accepted));

    // Progress of the new call arrives as NOTIFY; each one is answered.
    let event = [("event", "refer"), ("content-type", "message/sipfrag")];
    let ringing = in_dialog(
        &server,
        &invite,
        Method::Notify,
        1,
        &event,
        "SIP/2.0 180 Ringing\r\n",
    );
    server.send(&ringing, local);
    let ok = server.response(200);
    assert_eq!(ok.headers.get("cseq"), Some("1 NOTIFY"));

    let answered = in_dialog(
        &server,
        &invite,
        Method::Notify,
        2,
        &event,
        "SIP/2.0 200 OK\r\n",
    );
    server.send(&answered, local);
    wait_for(&handle, Event::Transfer(TransferState::Completed));

    // Our part of the call is over: the phone says goodbye by itself.
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("3 BYE"));
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn refused_or_failed_transfer_keeps_the_call() {
    let server = Server::new();
    let handle = phone(calm_timers());
    let (invite, _) = connect(&server, &handle);
    let local = handle.local_address();

    // The server will not transfer at all.
    assert!(handle.send(UiCommand::Transfer("4011".to_owned())));
    let (refer, peer) = server.request("REFER");
    server.reply(&refer, peer, 603, &[], None);
    assert_eq!(wait_error(&handle), "Transfer did not work: refused (603)");
    wait_for(&handle, Event::Transfer(TransferState::Failed(603)));

    // The server tries, but the new number is busy.
    assert!(handle.send(UiCommand::Transfer("4011".to_owned())));
    let (refer, peer) = server.request("REFER");
    assert_eq!(refer.headers.get("cseq"), Some("3 REFER"));
    server.reply(&refer, peer, 202, &[], None);
    wait_for(&handle, Event::Transfer(TransferState::Accepted));
    let busy = in_dialog(
        &server,
        &invite,
        Method::Notify,
        1,
        &[("event", "refer;id=3")],
        "SIP/2.0 486 Busy Here\r\n",
    );
    server.send(&busy, local);
    assert_eq!(
        wait_error(&handle),
        "Transfer did not work: the new call failed (486)"
    );
    wait_for(&handle, Event::Transfer(TransferState::Failed(486)));

    // The original call is still there to be ended normally.
    assert!(handle.send(UiCommand::Hangup));
    let (bye, peer) = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("4 BYE"));
    server.reply(&bye, peer, 200, &[], None);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn transfer_needs_a_connected_call_and_a_usable_number() {
    let handle = phone(fast_timers());
    assert!(handle.send(UiCommand::Transfer("4011".to_owned())));
    assert_eq!(
        wait_error(&handle),
        "only a connected call can be transferred"
    );

    let server = Server::new();
    let handle = phone(calm_timers());
    connect(&server, &handle);
    assert!(handle.send(UiCommand::Transfer("40 11".to_owned())));
    assert_eq!(wait_error(&handle), "invalid number or SIP address");
    assert!(
        !server
            .methods_for(Duration::from_millis(200))
            .contains(&"REFER".to_owned())
    );
}

#[test]
fn do_not_disturb_refuses_calls_without_ringing_and_reports_them() {
    let server = Server::new();
    let handle = phone(fast_timers());
    let local = handle.local_address();
    let address = server.address();
    assert!(handle.send(UiCommand::DoNotDisturb(true)));
    std::thread::sleep(Duration::from_millis(100));

    let call = |call_id: &str| {
        InviteRequest::new(
            format!("sip:phone@{local}"),
            format!("sip:4010@{address}"),
            format!("sip:phone@{local}"),
            format!("sip:4010@{address}"),
            address.to_string(),
            call_id,
            format!("z9hG4bK-{call_id}"),
            "caller-tag",
        )
        .body(SDP.as_bytes().to_vec())
        .build()
        .unwrap()
    };
    server.send(&call("dnd-call"), local);
    let busy = server.response(486);
    assert_eq!(busy.headers.get("call-id"), Some("dnd-call"));
    wait_for(
        &handle,
        Event::Declined {
            number: "4010".to_owned(),
            name: String::new(),
        },
    );

    // Switched off again, the next call is offered as usual.
    assert!(handle.send(UiCommand::DoNotDisturb(false)));
    // The command travels on its own channel; let it arrive before the call.
    std::thread::sleep(Duration::from_millis(100));
    server.send(&call("normal-call"), local);
    wait_for(&handle, Event::Incoming { peer: address });
}

#[test]
fn options_is_answered_so_the_server_sees_the_phone_alive() {
    let server = Server::new();
    let handle = phone(fast_timers());
    let local = handle.local_address();
    let address = server.address();

    let options = RequestBuilder::new(Method::Options, format!("sip:phone@{local}"))
        .header(
            "via",
            format!("SIP/2.0/UDP {address};branch=z9hG4bK-options"),
        )
        .unwrap()
        .header("from", format!("<sip:pbx@{address}>;tag=pbx"))
        .unwrap()
        .header("to", format!("<sip:phone@{local}>"))
        .unwrap()
        .header("call-id", "qualify-1")
        .unwrap()
        .header("cseq", "1 OPTIONS")
        .unwrap()
        .header("max-forwards", "70")
        .unwrap()
        .build()
        .unwrap();
    server.send(&options, local);
    let ok = server.response(200);
    assert_eq!(ok.headers.get("call-id"), Some("qualify-1"));
    assert_eq!(ok.headers.get("cseq"), Some("1 OPTIONS"));
    assert!(ok.headers.get("allow").unwrap().contains("INVITE"));
    assert!(ok.headers.get("to").unwrap().contains(";tag="));
}
