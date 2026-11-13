//! A phone with two lines against scripted peers on raw loopback sockets:
//! a second call while talking, switching between calls, joining the two
//! parties, a conference, forwarding, and a password asked on hang-up.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, TransferState, UiCommand, spawn_lines,
};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const LOOPBACK: &str = "127.0.0.1:0";
const SDP: &str = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\n\
                   t=0 0\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";

/// Slow enough that no retransmission disturbs a scripted exchange.
fn calm_timers() -> Timers {
    Timers {
        t1: Duration::from_secs(2),
        t2: Duration::from_secs(4),
        give_up: Duration::from_secs(8),
    }
}

fn phone() -> CallHandle {
    spawn_lines(
        LOOPBACK.parse().unwrap(),
        "phone",
        calm_timers(),
        2,
        NullMedia::default,
    )
    .expect("executor should start")
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

/// Collects events until `last` arrives; returns all of them, `last` included.
fn events_until(handle: &CallHandle, last: Event) -> Vec<Event> {
    let deadline = Instant::now() + WAIT;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        if let Some(event) = handle.wait_event(Duration::from_millis(50)) {
            let done = event == last;
            events.push(event);
            if done {
                return events;
            }
        }
    }
    panic!("timed out waiting for {last:?}; got {events:?}");
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

/// Someone the phone talks to: a raw socket and, once connected, a dialog.
struct Peer {
    name: &'static str,
    socket: UdpSocket,
    dialog: Option<EstablishedDialog>,
    invite: Option<Request>,
}

impl Peer {
    fn new(name: &'static str) -> Self {
        let socket = UdpSocket::bind(LOOPBACK).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        Self {
            name,
            socket,
            dialog: None,
            invite: None,
        }
    }

    fn address(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    fn receive(&self) -> Message {
        let mut buffer = [0_u8; 65_535];
        let (length, _) = self
            .socket
            .recv_from(&mut buffer)
            .unwrap_or_else(|_| panic!("{} expected a datagram", self.name));
        Message::parse(std::str::from_utf8(&buffer[..length]).unwrap()).unwrap()
    }

    fn request(&self, method: &str) -> Request {
        loop {
            if let Message::Request(request) = self.receive()
                && request.method.as_str() == method
            {
                return request;
            }
        }
    }

    fn response(&self, status: u16) -> Response {
        loop {
            if let Message::Response(response) = self.receive()
                && response.status_code == status
            {
                return response;
            }
        }
    }

    fn send(&self, request: &Request, target: SocketAddr) {
        self.socket
            .send_to(&request.serialize().unwrap(), target)
            .unwrap();
    }

    fn reply(&self, request: &Request, target: SocketAddr, status: u16, extra: &[(&str, &str)]) {
        self.reply_with(request, target, status, extra, None);
    }

    fn reply_with(
        &self,
        request: &Request,
        target: SocketAddr,
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
        let to = if to.contains(";tag=") {
            to.to_owned()
        } else {
            format!("{to};tag={}-tag", self.name)
        };
        builder = builder.header("to", to).unwrap();
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
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
        self.socket.send_to(&bytes, target).unwrap();
    }

    /// Calls the phone and waits until it rings there.
    fn call(&mut self, phone: SocketAddr) {
        let address = self.address();
        let invite = InviteRequest::new(
            format!("sip:phone@{phone}"),
            format!("sip:{}@{address}", self.name),
            format!("sip:phone@{phone}"),
            format!("sip:{}@{address}", self.name),
            address.to_string(),
            format!("call-from-{}", self.name),
            format!("z9hG4bK-{}", self.name),
            format!("{}-tag", self.name),
        )
        .body(SDP.as_bytes().to_vec())
        .build()
        .unwrap();
        self.send(&invite, phone);
        self.response(180);
        self.invite = Some(invite);
    }

    /// Takes the phone's 200 for the call made with [`Peer::call`] and
    /// acknowledges it.
    fn finish_call(&mut self, phone: SocketAddr) {
        let address = self.address();
        let ok = self.response(200);
        let mut dialog = EstablishedDialog::new(DialogConfig {
            remote_target: format!("sip:phone@{phone}"),
            local_from: format!("<sip:{}@{address}>;tag={}-tag", self.name, self.name),
            remote_to: ok.headers.get("to").unwrap().to_owned(),
            call_id: format!("call-from-{}", self.name),
            local_sent_by: address.to_string(),
            local_contact: format!("<sip:{}@{address}>", self.name),
            invite_cseq: 1,
            invite_branch: format!("z9hG4bK-{}", self.name),
        })
        .unwrap();
        let ack = dialog
            .build_success_ack(&format!("z9hG4bK-{}-ack", self.name))
            .unwrap();
        self.send(&ack, phone);
        self.dialog = Some(dialog);
    }

    /// Answers a hold or resume request of the phone and returns its SDP.
    fn accept_reinvite(&self, phone: SocketAddr) -> String {
        let reinvite = self.request("INVITE");
        self.reply_with(&reinvite, phone, 200, &[], Some(SDP));
        self.request("ACK");
        String::from_utf8(reinvite.body).unwrap()
    }
}

/// Two peers call the phone one after the other; both calls are answered.
/// Leaves the second call talking on line 2 and the first held on line 1.
fn two_calls(handle: &CallHandle) -> (Peer, Peer) {
    let local = handle.local_address();
    let mut first = Peer::new("first");
    let mut second = Peer::new("second");

    first.call(local);
    wait_for(
        handle,
        Event::Incoming {
            peer: first.address(),
        },
    );
    assert!(handle.send(UiCommand::Answer));
    first.finish_call(local);
    wait_for(handle, Event::Established);

    // The second call arrives while the first is talking.
    second.call(local);
    let ringing = events_until(
        handle,
        Event::Incoming {
            peer: second.address(),
        },
    );
    assert!(ringing.contains(&Event::Line(1)), "{ringing:?}");

    // Answering it selects its line and puts the first call on hold.
    assert!(handle.send(UiCommand::Answer));
    second.finish_call(local);
    let hold = first.accept_reinvite(local);
    assert!(hold.contains("a=sendonly\r\n"), "{hold}");
    let answered = events_until(handle, Event::Hold(true));
    assert!(answered.contains(&Event::Selected(1)), "{answered:?}");
    assert!(answered.contains(&Event::Established), "{answered:?}");
    // The hold is reported for line 1, the connection for line 2.
    // The line is announced only when it changes, so look back over
    // everything since the second call began to ring.
    let all: Vec<Event> = ringing.iter().chain(&answered).cloned().collect();
    let line_before = |wanted: &Event| {
        let position = all.iter().rposition(|event| event == wanted).unwrap();
        all[..position]
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::Line(line) => Some(*line),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(line_before(&Event::Established), 1);
    assert_eq!(line_before(&Event::Hold(true)), 0);
    (first, second)
}

#[test]
fn second_call_is_taken_on_the_other_line_and_the_first_is_held() {
    let handle = phone();
    let (first, second) = two_calls(&handle);
    let local = handle.local_address();

    // Back to the first call: it is resumed and the second one is held.
    assert!(handle.send(UiCommand::SelectLine(0)));
    wait_for(&handle, Event::Selected(0));
    assert!(handle.send(UiCommand::Hold(false)));
    let resume = first.accept_reinvite(local);
    assert!(resume.contains("a=sendrecv\r\n"), "{resume}");
    let hold = second.accept_reinvite(local);
    assert!(hold.contains("a=sendonly\r\n"), "{hold}");
    wait_for(&handle, Event::Hold(true));

    // Hanging up ends the selected call only.
    assert!(handle.send(UiCommand::Hangup));
    let bye = first.request("BYE");
    first.reply(&bye, local, 200, &[]);
    wait_for(&handle, Event::Ended(EndReason::Completed));

    assert!(handle.send(UiCommand::SelectLine(1)));
    assert!(handle.send(UiCommand::Hangup));
    let bye = second.request("BYE");
    second.reply(&bye, local, 200, &[]);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}

#[test]
fn third_call_finds_both_lines_busy() {
    let handle = phone();
    let (_first, _second) = two_calls(&handle);
    let local = handle.local_address();

    let third = Peer::new("third");
    let address = third.address();
    let invite = InviteRequest::new(
        format!("sip:phone@{local}"),
        format!("sip:third@{address}"),
        format!("sip:phone@{local}"),
        format!("sip:third@{address}"),
        address.to_string(),
        "call-from-third",
        "z9hG4bK-third",
        "third-tag",
    )
    .body(SDP.as_bytes().to_vec())
    .build()
    .unwrap();
    third.send(&invite, local);
    assert_eq!(
        third.response(486).headers.get("call-id"),
        Some("call-from-third")
    );
}

#[test]
fn new_call_from_a_busy_phone_uses_the_free_line() {
    let handle = phone();
    let local = handle.local_address();
    let mut first = Peer::new("first");
    first.call(local);
    wait_for(
        &handle,
        Event::Incoming {
            peer: first.address(),
        },
    );
    assert!(handle.send(UiCommand::Answer));
    first.finish_call(local);
    wait_for(&handle, Event::Established);

    // Dialling while talking: line 2 is used and line 1 is held.
    let second = Peer::new("second");
    assert!(handle.send(UiCommand::Dial(second.address())));
    let invite = second.request("INVITE");
    assert_ne!(
        invite.headers.get("call-id"),
        Some("call-from-first"),
        "a new call, not a change of the first"
    );
    let hold = first.accept_reinvite(local);
    assert!(hold.contains("a=sendonly\r\n"), "{hold}");
    let events = events_until(&handle, Event::Hold(true));
    assert!(events.contains(&Event::Selected(1)), "{events:?}");
}

#[test]
fn joining_two_calls_refers_one_party_to_the_other() {
    let handle = phone();
    let (first, second) = two_calls(&handle);
    let local = handle.local_address();

    // Line 2 (the second call) is selected: its party is connected with
    // the party of line 1.
    assert!(handle.send(UiCommand::TransferToLine(0)));
    let refer = first.request("REFER");
    assert_eq!(refer.headers.get("call-id"), Some("call-from-first"));
    let refer_to = refer.headers.get("refer-to").unwrap();
    // Refer-To names the second party and, in Replaces, the call to it:
    // Call-ID, then the second party's tag and ours.
    let our_tag = second
        .dialog
        .as_ref()
        .map(|_| ())
        .and(refer_to.split("from-tag%3D").nth(1))
        .unwrap()
        .trim_end_matches('>');
    assert_eq!(
        refer_to,
        format!(
            "<sip:second@{}?Replaces=call-from-second%3Bto-tag%3Dsecond-tag%3Bfrom-tag%3D{our_tag}>",
            second.address()
        )
    );
    assert!(our_tag.starts_with("rs-"), "{our_tag}");
    first.reply(&refer, local, 202, &[]);
    wait_for(&handle, Event::Transfer(TransferState::Accepted));
    drop(second);
}

#[test]
fn joining_needs_two_connected_calls() {
    let handle = phone();
    assert!(handle.send(UiCommand::TransferToLine(1)));
    assert_eq!(wait_error(&handle), "both lines must have a connected call");
    assert!(handle.send(UiCommand::TransferToLine(0)));
    assert_eq!(wait_error(&handle), "choose the other line to connect with");
    assert!(handle.send(UiCommand::SelectLine(5)));
    assert_eq!(wait_error(&handle), "this phone has no line 6");
}

#[test]
fn conference_resumes_the_held_call_and_separating_holds_it_again() {
    let handle = phone();
    let (first, _second) = two_calls(&handle);
    let local = handle.local_address();

    assert!(handle.send(UiCommand::Conference(true)));
    let resume = first.accept_reinvite(local);
    assert!(resume.contains("a=sendrecv\r\n"), "{resume}");
    let events = events_until(&handle, Event::Hold(false));
    assert!(events.contains(&Event::Conference(true)), "{events:?}");

    // Leaving the conference: only the selected line keeps talking.
    assert!(handle.send(UiCommand::Conference(false)));
    let hold = first.accept_reinvite(local);
    assert!(hold.contains("a=sendonly\r\n"), "{hold}");
    let events = events_until(&handle, Event::Hold(true));
    assert!(events.contains(&Event::Conference(false)), "{events:?}");
}

#[test]
fn forwarding_sends_the_caller_to_another_number() {
    let handle = phone();
    let local = handle.local_address();
    assert!(handle.send(UiCommand::Forward(Some(" 4011 ".to_owned()))));
    std::thread::sleep(Duration::from_millis(100));

    let caller = Peer::new("caller");
    let address = caller.address();
    let invite = |call_id: &str| {
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
    caller.send(&invite("forwarded"), local);
    let moved = caller.response(302);
    assert_eq!(
        moved.headers.get("contact"),
        Some(format!("<sip:4011@{address}>").as_str())
    );
    wait_for(
        &handle,
        Event::Forwarded {
            number: "4010".to_owned(),
            name: String::new(),
            to: "4011".to_owned(),
        },
    );

    // Switched off, calls ring here again.
    assert!(handle.send(UiCommand::Forward(None)));
    std::thread::sleep(Duration::from_millis(100));
    caller.send(&invite("not-forwarded"), local);
    wait_for(&handle, Event::Incoming { peer: address });
}

#[test]
fn password_asked_on_hang_up_is_answered_once() {
    const CHALLENGE: &str = "Digest realm=\"pbx\", nonce=\"bye-nonce\", qop=\"auth\"";
    let handle = phone();
    let local = handle.local_address();
    let server = Peer::new("server");
    let address = server.address();

    // Register without a challenge, then call a number through the server.
    let account = Account::new(address.to_string(), "alice", "secret");
    assert!(handle.send(UiCommand::Register(account)));
    let register = server.request("REGISTER");
    server.reply(&register, local, 200, &[("expires", "3600")]);
    wait_for(
        &handle,
        Event::Registration(RegistrationState::Registered { expires: 3600 }),
    );
    assert!(handle.send(UiCommand::DialTarget("100".to_owned())));
    let invite = server.request("INVITE");
    let contact = format!("<sip:100@{address}>");
    server.reply_with(
        &invite,
        local,
        200,
        &[("contact", contact.as_str())],
        Some(SDP),
    );
    server.request("ACK");
    wait_for(&handle, Event::Established);

    assert!(handle.send(UiCommand::Hangup));
    let bye = server.request("BYE");
    assert_eq!(bye.headers.get("cseq"), Some("2 BYE"));
    assert!(bye.headers.get("authorization").is_none());
    server.reply(&bye, local, 401, &[("www-authenticate", CHALLENGE)]);

    // The same BYE again as a new request, now with credentials.
    let again = server.request("BYE");
    assert_eq!(again.headers.get("cseq"), Some("3 BYE"));
    assert_eq!(again.headers.get("call-id"), bye.headers.get("call-id"));
    assert_ne!(again.headers.get("via"), bye.headers.get("via"));
    let authorization = again.headers.get("authorization").unwrap();
    assert!(authorization.contains("username=\"alice\""));
    assert!(authorization.contains("nonce=\"bye-nonce\""));
    assert!(authorization.contains(&format!("uri=\"{}\"", again.target)));

    // A second challenge is not answered again: the call simply ends.
    server.reply(&again, local, 401, &[("www-authenticate", CHALLENGE)]);
    wait_for(&handle, Event::Ended(EndReason::Completed));
}
