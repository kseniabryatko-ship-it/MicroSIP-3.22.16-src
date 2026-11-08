use microsip_core::call_media::{Media, MediaError, NullMedia};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::sip::message::{InviteRequest, Message, Request, ResponseBuilder};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
const LOOPBACK: &str = "127.0.0.1:0";

#[derive(Clone, Default)]
struct Counters {
    started: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
}

struct CountingMedia {
    inner: NullMedia,
    counters: Counters,
    on: bool,
}

impl CountingMedia {
    fn new(counters: Counters) -> Self {
        Self {
            inner: NullMedia::default(),
            counters,
            on: false,
        }
    }
}

impl Media for CountingMedia {
    fn prepare(&mut self, ip: IpAddr) -> Result<SocketAddr, MediaError> {
        self.inner.prepare(ip)
    }

    fn start(&mut self, remote: SocketAddr) -> Result<(), MediaError> {
        self.inner.start(remote)?;
        self.on = true;
        self.counters.started.fetch_add(1, Ordering::SeqCst);
        self.counters.running.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&mut self) {
        self.inner.stop();
        if self.on {
            self.on = false;
            self.counters.running.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn phone(timers: Timers) -> (CallHandle, Counters) {
    let counters = Counters::default();
    let for_media = counters.clone();
    let handle = spawn_with_timers(LOOPBACK.parse().unwrap(), "phone", timers, move || {
        CountingMedia::new(for_media)
    })
    .expect("executor should start");
    (handle, counters)
}

fn default_phone() -> (CallHandle, Counters) {
    phone(Timers::default())
}

fn fast_timers() -> Timers {
    Timers {
        t1: Duration::from_millis(30),
        t2: Duration::from_millis(100),
        give_up: Duration::from_millis(600),
    }
}

/// Waits for `expected`, failing on any reported error or on timeout.
fn wait_for(handle: &CallHandle, expected: Event) {
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        match handle.wait_event(Duration::from_millis(50)) {
            Some(event) if event == expected => return,
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    panic!("timed out waiting for {expected:?}");
}

fn scripted_socket() -> UdpSocket {
    let socket = UdpSocket::bind(LOOPBACK).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    socket
}

fn receive(socket: &UdpSocket) -> (Message, SocketAddr) {
    let mut buffer = [0_u8; 65_535];
    let (length, source) = socket.recv_from(&mut buffer).expect("expected a datagram");
    let text = std::str::from_utf8(&buffer[..length]).unwrap();
    (Message::parse(text).unwrap(), source)
}

fn receive_request(socket: &UdpSocket, method: &str) -> Request {
    loop {
        if let (Message::Request(request), _) = receive(socket)
            && request.method.as_str() == method
        {
            return request;
        }
    }
}

fn reply(request: &Request, status: u16, reason: &str) -> Vec<u8> {
    let to = request.headers.get("to").unwrap();
    let to = if to.contains(";tag=") {
        to.to_owned()
    } else {
        format!("{to};tag=scripted-tag")
    };
    let mut builder = ResponseBuilder::new(status, reason);
    for via in request.headers.get_all("via") {
        builder = builder.append_header("via", via).unwrap();
    }
    builder
        .header("from", request.headers.get("from").unwrap())
        .unwrap()
        .header("to", to)
        .unwrap()
        .header("call-id", request.headers.get("call-id").unwrap())
        .unwrap()
        .header("cseq", request.headers.get("cseq").unwrap())
        .unwrap()
        .header("contact", "<sip:scripted@127.0.0.1:9>")
        .unwrap()
        .build()
        .unwrap()
        .serialize()
        .unwrap()
}

fn scripted_invite(socket: &UdpSocket, target: SocketAddr, call_id: &str) -> Request {
    let local = socket.local_addr().unwrap();
    let sdp = "v=0\r\no=raw 1 1 IN IP4 127.0.0.1\r\ns=raw\r\nc=IN IP4 127.0.0.1\r\n\
               t=0 0\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    InviteRequest::new(
        format!("sip:phone@{target}"),
        format!("sip:raw@{local}"),
        format!("sip:phone@{target}"),
        format!("sip:raw@{local}"),
        local.to_string(),
        call_id,
        format!("z9hG4bK-{call_id}"),
        "raw-tag",
    )
    .body(sdp.as_bytes().to_vec())
    .build()
    .unwrap()
}

fn send(socket: &UdpSocket, request: &Request, target: SocketAddr) {
    socket
        .send_to(&request.serialize().unwrap(), target)
        .unwrap();
}

fn status_of(message: &Message) -> u16 {
    match message {
        Message::Response(response) => response.status_code,
        Message::Request(_) => panic!("expected a response"),
    }
}

fn connect(a: &CallHandle, b: &CallHandle) {
    a.send(UiCommand::Dial(b.local_address()));
    wait_for(
        b,
        Event::Incoming {
            peer: a.local_address(),
        },
    );
    wait_for(a, Event::Ringing);
    b.send(UiCommand::Answer);
    wait_for(a, Event::Established);
    wait_for(b, Event::Established);
}

#[test]
fn caller_hangup_ends_both_sides() {
    let (a, a_media) = default_phone();
    let (b, b_media) = default_phone();
    connect(&a, &b);
    assert_eq!(a_media.running.load(Ordering::SeqCst), 1);
    assert_eq!(b_media.running.load(Ordering::SeqCst), 1);

    a.send(UiCommand::Hangup);
    wait_for(&a, Event::Ended(EndReason::Completed));
    wait_for(&b, Event::Ended(EndReason::Completed));
    assert_eq!(a_media.started.load(Ordering::SeqCst), 1);
    assert_eq!(b_media.started.load(Ordering::SeqCst), 1);
    assert_eq!(a_media.running.load(Ordering::SeqCst), 0);
    assert_eq!(b_media.running.load(Ordering::SeqCst), 0);
}

#[test]
fn callee_hangup_ends_both_sides() {
    let (a, _) = default_phone();
    let (b, _) = default_phone();
    connect(&a, &b);

    b.send(UiCommand::Hangup);
    wait_for(&b, Event::Ended(EndReason::Completed));
    wait_for(&a, Event::Ended(EndReason::Completed));
}

#[test]
fn rejection_ends_both_sides_and_allows_a_new_call() {
    let (a, a_media) = default_phone();
    let (b, _) = default_phone();
    a.send(UiCommand::Dial(b.local_address()));
    wait_for(
        &b,
        Event::Incoming {
            peer: a.local_address(),
        },
    );
    b.send(UiCommand::Reject);
    wait_for(&a, Event::Ended(EndReason::Rejected(486)));
    wait_for(&b, Event::Ended(EndReason::Rejected(486)));
    assert_eq!(a_media.started.load(Ordering::SeqCst), 0);

    connect(&a, &b);
    a.send(UiCommand::Hangup);
    wait_for(&a, Event::Ended(EndReason::Completed));
    wait_for(&b, Event::Ended(EndReason::Completed));
}

#[test]
fn cancel_while_ringing_ends_both_sides() {
    let (a, _) = default_phone();
    let (b, _) = default_phone();
    a.send(UiCommand::Dial(b.local_address()));
    wait_for(
        &b,
        Event::Incoming {
            peer: a.local_address(),
        },
    );
    wait_for(&a, Event::Ringing);

    a.send(UiCommand::Hangup);
    wait_for(&a, Event::Ended(EndReason::Cancelled));
    wait_for(&b, Event::Ended(EndReason::Cancelled));
}

#[test]
fn caller_retransmits_invite_and_acks_failure_with_original_branch() {
    let scripted = scripted_socket();
    let (a, _) = phone(Timers {
        give_up: Duration::from_secs(5),
        ..fast_timers()
    });
    a.send(UiCommand::Dial(scripted.local_addr().unwrap()));

    let first = receive_request(&scripted, "INVITE");
    let again = receive_request(&scripted, "INVITE");
    assert_eq!(first, again, "INVITE must be retransmitted unchanged");

    scripted
        .send_to(&reply(&first, 486, "Busy Here"), a.local_address())
        .unwrap();
    let ack = receive_request(&scripted, "ACK");
    assert_eq!(ack.headers.get("cseq"), Some("1 ACK"));
    assert_eq!(ack.headers.get("via"), first.headers.get("via"));
    wait_for(&a, Event::Ended(EndReason::Rejected(486)));

    // The final response is repeated when the ACK was lost: ACK again.
    scripted
        .send_to(&reply(&first, 486, "Busy Here"), a.local_address())
        .unwrap();
    let ack_again = receive_request(&scripted, "ACK");
    assert_eq!(ack_again, ack);
}

#[test]
fn unanswered_invite_times_out() {
    let silent = scripted_socket();
    let (a, a_media) = phone(fast_timers());
    a.send(UiCommand::Dial(silent.local_addr().unwrap()));
    wait_for(&a, Event::Ended(EndReason::Timeout));
    assert_eq!(a_media.started.load(Ordering::SeqCst), 0);
}

#[test]
fn called_side_follows_the_wire_protocol_for_a_scripted_caller() {
    let scripted = scripted_socket();
    let (b, b_media) = default_phone();
    let target = b.local_address();

    let invite = scripted_invite(&scripted, target, "scripted-call");
    send(&scripted, &invite, target);
    assert_eq!(status_of(&receive(&scripted).0), 100);
    assert_eq!(status_of(&receive(&scripted).0), 180);
    wait_for(
        &b,
        Event::Incoming {
            peer: scripted.local_addr().unwrap(),
        },
    );

    // A retransmitted INVITE gets the latest response again, not a new call.
    send(&scripted, &invite, target);
    assert_eq!(status_of(&receive(&scripted).0), 180);

    // A different INVITE while busy is refused.
    let other = scripted_invite(&scripted, target, "other-call");
    send(&scripted, &other, target);
    assert_eq!(status_of(&receive(&scripted).0), 486);

    b.send(UiCommand::Answer);
    let Message::Response(ok) = receive(&scripted).0 else {
        panic!("expected 200 OK");
    };
    assert_eq!(ok.status_code, 200);
    assert!(std::str::from_utf8(&ok.body).unwrap().contains("m=audio"));

    let contact = ok.headers.get("contact").unwrap();
    let mut dialog = EstablishedDialog::new(DialogConfig {
        remote_target: contact
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
            .unwrap()
            .to_owned(),
        local_from: invite.headers.get("from").unwrap().to_owned(),
        remote_to: ok.headers.get("to").unwrap().to_owned(),
        call_id: "scripted-call".to_owned(),
        local_sent_by: scripted.local_addr().unwrap().to_string(),
        local_contact: format!("<sip:raw@{}>", scripted.local_addr().unwrap()),
        invite_cseq: 1,
        invite_branch: "z9hG4bK-scripted-call".to_owned(),
    })
    .unwrap();
    send(
        &scripted,
        &dialog.build_success_ack("z9hG4bK-scripted-ack").unwrap(),
        target,
    );
    wait_for(&b, Event::Established);
    assert_eq!(b_media.running.load(Ordering::SeqCst), 1);

    let bye = dialog.build_bye("z9hG4bK-scripted-bye").unwrap();
    send(&scripted, &bye, target);
    let Message::Response(done) = receive(&scripted).0 else {
        panic!("expected a response to BYE");
    };
    assert_eq!(done.status_code, 200);
    assert_eq!(done.headers.get("cseq"), Some("2 BYE"));
    wait_for(&b, Event::Ended(EndReason::Completed));
    assert_eq!(b_media.running.load(Ordering::SeqCst), 0);

    // A repeated BYE gets the same answer instead of an error.
    send(&scripted, &bye, target);
    assert_eq!(status_of(&receive(&scripted).0), 200);
}

#[test]
fn called_side_sends_bye_when_the_ack_never_arrives() {
    let scripted = scripted_socket();
    let (b, b_media) = phone(fast_timers());
    let target = b.local_address();

    send(
        &scripted,
        &scripted_invite(&scripted, target, "no-ack-call"),
        target,
    );
    wait_for(
        &b,
        Event::Incoming {
            peer: scripted.local_addr().unwrap(),
        },
    );
    b.send(UiCommand::Answer);

    let bye = receive_request(&scripted, "BYE");
    scripted.send_to(&reply(&bye, 200, "OK"), target).unwrap();
    wait_for(&b, Event::Ended(EndReason::Timeout));
    assert_eq!(b_media.started.load(Ordering::SeqCst), 0);
}

#[test]
fn dropping_the_handle_hangs_up_an_active_call() {
    let (a, _) = default_phone();
    let (b, _) = default_phone();
    connect(&a, &b);
    drop(a);
    wait_for(&b, Event::Ended(EndReason::Completed));
}

#[test]
fn invalid_destination_reports_an_error_and_a_failed_call() {
    let (a, _) = default_phone();
    a.send(UiCommand::Dial("0.0.0.0:5060".parse().unwrap()));
    assert!(matches!(
        a.wait_event(WAIT),
        Some(Event::Error(message)) if message.contains("invalid destination")
    ));
    assert_eq!(a.wait_event(WAIT), Some(Event::Ended(EndReason::Failed)));

    // The executor is still usable afterwards.
    let (b, _) = default_phone();
    connect(&a, &b);
    a.send(UiCommand::Hangup);
    wait_for(&a, Event::Ended(EndReason::Completed));
}

#[test]
fn media_counters_are_reported_when_audio_stops() {
    let (a, _) = default_phone();
    let (b, _) = default_phone();
    connect(&a, &b);
    a.send(UiCommand::Hangup);
    wait_for(
        &a,
        Event::MediaStats {
            sent: 0,
            received: 0,
        },
    );
    wait_for(&a, Event::Ended(EndReason::Completed));
}
