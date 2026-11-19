//! Voicemail, busy lamps and text messages through the executor, against a
//! scripted server on a raw loopback socket. No sound devices are used.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{CallHandle, Event, Timers, UiCommand, spawn_with_timers};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::services::{LineState, Voicemail};
use microsip_core::sip::message::{Message, Method, Request, RequestBuilder, ResponseBuilder};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const CHALLENGE: &str = "Digest realm=\"pbx\", nonce=\"svc-nonce\", algorithm=MD5";

fn phone() -> CallHandle {
    spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        // Slow enough that no retransmission disturbs a scripted exchange.
        Timers {
            t1: Duration::from_secs(2),
            t2: Duration::from_secs(4),
            give_up: Duration::from_secs(8),
        },
        NullMedia::default,
    )
    .expect("executor should start")
}

struct Server {
    socket: UdpSocket,
}

impl Server {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
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
    fn response(&self, status: u16) -> microsip_core::sip::message::Response {
        loop {
            if let (Message::Response(response), _) = self.receive()
                && response.status_code == status
            {
                return response;
            }
        }
    }

    /// True when nothing at all arrives within `window`.
    fn silent_for(&self, window: Duration) -> bool {
        self.socket.set_read_timeout(Some(window)).unwrap();
        let mut buffer = [0_u8; 65_535];
        let quiet = matches!(
            self.socket.recv_from(&mut buffer),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
        );
        self.socket.set_read_timeout(Some(WAIT)).unwrap();
        quiet
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
        let to = request.headers.get("to").unwrap();
        let to = if to.contains(";tag=") {
            to.to_owned()
        } else {
            format!("{to};tag=server-tag")
        };
        builder = builder.header("to", to).unwrap();
        for (name, value) in extra {
            builder = builder.header(name, *value).unwrap();
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

    /// Registers the phone and accepts the mailbox subscription that
    /// follows. Returns that SUBSCRIBE.
    fn register(&self, handle: &CallHandle) -> (Request, SocketAddr) {
        let account = Account::new(self.address().to_string(), "4009", "secret");
        assert!(handle.send(UiCommand::Register(account)));
        let (request, peer) = self.request("REGISTER");
        self.reply(&request, peer, 200, &[("expires", "3600")]);
        wait_for(handle, |event| {
            matches!(
                event,
                Event::Registration(RegistrationState::Registered { .. })
            )
        });
        let (subscribe, peer) = self.request("SUBSCRIBE");
        self.reply(&subscribe, peer, 200, &[("expires", "600")]);
        (subscribe, peer)
    }
}

/// A NOTIFY inside the subscription made by `subscribe`.
fn notify(subscribe: &Request, cseq: u32, state: &str, kind: &str, body: &str) -> Request {
    let from_tag = subscribe
        .headers
        .get("from")
        .unwrap()
        .split("tag=")
        .nth(1)
        .unwrap()
        .to_owned();
    RequestBuilder::new(
        Method::Notify,
        subscribe
            .headers
            .get("contact")
            .unwrap()
            .trim_matches(['<', '>']),
    )
    .header(
        "via",
        format!("SIP/2.0/UDP pbx;branch=z9hG4bK-notify-{cseq}"),
    )
    .unwrap()
    .header("max-forwards", "70")
    .unwrap()
    .header(
        "from",
        format!(
            "{};tag=server-tag",
            subscribe
                .headers
                .get("to")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
        ),
    )
    .unwrap()
    .header("to", format!("<sip:4009@pbx>;tag={from_tag}"))
    .unwrap()
    .header("call-id", subscribe.headers.get("call-id").unwrap())
    .unwrap()
    .header("cseq", format!("{cseq} NOTIFY"))
    .unwrap()
    .header("event", subscribe.headers.get("event").unwrap())
    .unwrap()
    .header("subscription-state", state)
    .unwrap()
    .header("content-type", kind)
    .unwrap()
    .body(body.as_bytes().to_vec())
    .build()
    .unwrap()
}

fn wait_for(handle: &CallHandle, wanted: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match handle.wait_event(Duration::from_millis(50)) {
            Some(event) if wanted(&event) => return event,
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    panic!("timed out");
}

const SUMMARY: &str = "application/simple-message-summary";
const DIALOG: &str = "application/dialog-info+xml";

fn dialog_info(state: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<dialog-info xmlns=\"urn:ietf:params:xml:ns:dialog-info\" \
         version=\"1\" state=\"full\" entity=\"sip:4010@pbx\">\n\
         <dialog id=\"x\" direction=\"recipient\"><state>{state}</state></dialog>\n</dialog-info>"
    )
}

#[test]
fn the_mailbox_is_watched_once_registered_and_its_news_reported() {
    let server = Server::new();
    let handle = phone();
    let account = Account::new(server.address().to_string(), "4009", "secret");
    assert!(handle.send(UiCommand::Register(account)));
    let (register, peer) = server.request("REGISTER");
    server.reply(&register, peer, 200, &[("expires", "3600")]);

    // The phone subscribes to its own mailbox and gives the password when
    // asked.
    let (first, peer) = server.request("SUBSCRIBE");
    assert_eq!(first.target, format!("sip:4009@{}", server.address().ip()));
    assert_eq!(first.headers.get("event"), Some("message-summary"));
    assert!(first.headers.get("authorization").is_none());
    server.reply(&first, peer, 401, &[("www-authenticate", CHALLENGE)]);
    let (second, peer) = server.request("SUBSCRIBE");
    let credentials = second.headers.get("authorization").unwrap();
    assert!(credentials.contains("username=\"4009\""), "{credentials}");
    assert!(credentials.contains("nonce=\"svc-nonce\""), "{credentials}");
    server.reply(&second, peer, 200, &[("expires", "600")]);

    // News of the mailbox is answered and reported.
    let news = notify(
        &second,
        1,
        "active;expires=600",
        SUMMARY,
        "Messages-Waiting: yes\r\nVoice-Message: 3/1 (0/0)\r\n",
    );
    server.send(&news, peer);
    let ok = server.response(200);
    assert_eq!(ok.headers.get("call-id"), news.headers.get("call-id"));
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::Voicemail(_))),
        Event::Voicemail(Voicemail {
            waiting: true,
            new: 3,
            old: 1
        })
    );

    // A server may also report without a subscription.
    let unsolicited = RequestBuilder::new(Method::Notify, format!("sip:4009@{peer}"))
        .header("via", "SIP/2.0/UDP pbx;branch=z9hG4bK-unsolicited")
        .unwrap()
        .header("max-forwards", "70")
        .unwrap()
        .header("from", "<sip:4009@pbx>;tag=mwi")
        .unwrap()
        .header("to", "<sip:4009@pbx>")
        .unwrap()
        .header("call-id", "unsolicited-mwi")
        .unwrap()
        .header("cseq", "5 NOTIFY")
        .unwrap()
        .header("event", "message-summary")
        .unwrap()
        .header("content-type", SUMMARY)
        .unwrap()
        .body(b"Messages-Waiting: no\r\nVoice-Message: 0/4 (0/0)\r\n".to_vec())
        .build()
        .unwrap();
    server.send(&unsolicited, peer);
    server.response(200);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::Voicemail(_))),
        Event::Voicemail(Voicemail {
            waiting: false,
            new: 0,
            old: 4
        })
    );

    // Taking the account off ends the subscription too.
    assert!(handle.send(UiCommand::Unregister));
    let (end, _) = server.request("SUBSCRIBE");
    assert_eq!(end.headers.get("expires"), Some("0"));
    assert_eq!(end.headers.get("call-id"), second.headers.get("call-id"));
}

#[test]
fn watched_extensions_light_up_and_can_be_changed() {
    let server = Server::new();
    let handle = phone();
    server.register(&handle);

    assert!(handle.send(UiCommand::Watch(vec![
        "4010".to_owned(),
        " 4010 ".to_owned(),
        "4011".to_owned(),
    ])));
    // One subscription per extension, repeats dropped.
    let (first, peer) = server.request("SUBSCRIBE");
    let (second, _) = server.request("SUBSCRIBE");
    let ip = server.address().ip();
    let mut targets = vec![first.target.clone(), second.target.clone()];
    targets.sort();
    assert_eq!(
        targets,
        [format!("sip:4010@{ip}"), format!("sip:4011@{ip}")]
    );
    assert_eq!(first.headers.get("event"), Some("dialog"));
    let (lamp, other) = if first.target.contains("4010") {
        (first, second)
    } else {
        (second, first)
    };
    server.reply(&lamp, peer, 200, &[("expires", "600")]);
    // The other one does not exist on this server.
    server.reply(&other, peer, 404, &[]);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::Presence { .. })),
        Event::Presence {
            number: "4011".to_owned(),
            state: LineState::Unknown
        }
    );

    for (cseq, xml_state, state) in [
        (1, "terminated", LineState::Idle),
        (2, "early", LineState::Ringing),
        (3, "confirmed", LineState::Busy),
    ] {
        server.send(
            &notify(&lamp, cseq, "active", DIALOG, &dialog_info(xml_state)),
            peer,
        );
        server.response(200);
        assert_eq!(
            wait_for(&handle, |event| matches!(event, Event::Presence { .. })),
            Event::Presence {
                number: "4010".to_owned(),
                state
            }
        );
    }

    // Nobody is watched any more: the server is told to stop.
    assert!(handle.send(UiCommand::Watch(Vec::new())));
    let (end, _) = server.request("SUBSCRIBE");
    assert_eq!(end.headers.get("expires"), Some("0"));
    assert_eq!(end.headers.get("call-id"), lamp.headers.get("call-id"));
    assert!(server.silent_for(Duration::from_millis(300)));
}

fn incoming_message(peer: SocketAddr, branch: &str, kind: &str, text: &str) -> Request {
    RequestBuilder::new(Method::Message, format!("sip:4009@{peer}"))
        .header("via", format!("SIP/2.0/UDP pbx;branch={branch}"))
        .unwrap()
        .header("max-forwards", "70")
        .unwrap()
        .header("from", "\"Анна\" <sip:4010@pbx>;tag=anna")
        .unwrap()
        .header("to", "<sip:4009@pbx>")
        .unwrap()
        .header("call-id", format!("msg-{branch}"))
        .unwrap()
        .header("cseq", "1 MESSAGE")
        .unwrap()
        .header("content-type", kind)
        .unwrap()
        .body(text.as_bytes().to_vec())
        .build()
        .unwrap()
}

#[test]
fn text_messages_go_out_with_a_password_and_come_in_once() {
    let server = Server::new();
    let handle = phone();

    // Without an account there is nobody to send through.
    assert!(handle.send(UiCommand::SendMessage {
        to: "4010".to_owned(),
        text: "hello".to_owned(),
    }));
    let deadline = Instant::now() + WAIT;
    let refused = loop {
        assert!(Instant::now() < deadline, "no error was reported");
        if let Some(Event::Error(text)) = handle.wait_event(Duration::from_millis(50)) {
            break text;
        }
    };
    assert!(refused.contains("register"), "{refused}");

    let (_, peer) = server.register(&handle);
    assert!(handle.send(UiCommand::SendMessage {
        to: "4010".to_owned(),
        text: "Привет!\nВторая строка".to_owned(),
    }));
    let (first, peer_seen) = server.request("MESSAGE");
    assert_eq!(peer_seen, peer);
    assert_eq!(first.target, format!("sip:4010@{}", server.address().ip()));
    assert_eq!(
        first.headers.get("content-type"),
        Some("text/plain;charset=UTF-8")
    );
    assert_eq!(first.body, "Привет!\nВторая строка".as_bytes());
    server.reply(&first, peer, 407, &[("proxy-authenticate", CHALLENGE)]);
    let (second, _) = server.request("MESSAGE");
    assert!(second.headers.get("proxy-authorization").is_some());
    assert_eq!(second.headers.get("cseq"), Some("2 MESSAGE"));
    server.reply(&second, peer, 202, &[]);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::MessageSent { .. })),
        Event::MessageSent {
            to: "4010".to_owned(),
            error: None
        }
    );

    // Refused by the server: the user is told why.
    assert!(handle.send(UiCommand::SendMessage {
        to: "4099".to_owned(),
        text: "anyone?".to_owned(),
    }));
    let (lost, _) = server.request("MESSAGE");
    server.reply(&lost, peer, 404, &[]);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::MessageSent { .. })),
        Event::MessageSent {
            to: "4099".to_owned(),
            error: Some("404 Scripted".to_owned())
        }
    );

    // A message arrives; its repetition is answered again but shown once.
    let message = incoming_message(peer, "z9hG4bK-m1", "text/plain;charset=UTF-8", "Как дела?");
    server.send(&message, peer);
    server.response(200);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::Message { .. })),
        Event::Message {
            number: "4010".to_owned(),
            name: "Анна".to_owned(),
            text: "Как дела?".to_owned()
        }
    );
    server.send(&message, peer);
    server.response(200);
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        if let Some(event) = handle.wait_event(Duration::from_millis(50)) {
            assert!(!matches!(event, Event::Message { .. }), "shown twice");
        }
    }

    // What is not text is refused.
    let picture = incoming_message(peer, "z9hG4bK-m2", "image/png", "\u{89}PNG");
    server.send(&picture, peer);
    server.response(415);
}

#[test]
fn the_own_status_is_published_and_colleagues_statuses_are_shown() {
    use microsip_core::presence::PresenceStatus;
    let server = Server::new();
    let handle = phone();
    let (_, peer) = server.register(&handle);

    // The status goes out with the password the server asks for.
    assert!(handle.send(UiCommand::Presence(Some(PresenceStatus::Away))));
    let (first, _) = server.request("PUBLISH");
    assert_eq!(first.target, format!("sip:4009@{}", server.address().ip()));
    assert_eq!(first.headers.get("event"), Some("presence"));
    assert_eq!(
        first.headers.get("content-type"),
        Some("application/pidf+xml")
    );
    assert!(String::from_utf8_lossy(&first.body).contains("<rpid:away/>"));
    server.reply(&first, peer, 401, &[("www-authenticate", CHALLENGE)]);
    let (second, _) = server.request("PUBLISH");
    assert!(second.headers.get("authorization").is_some());
    server.reply(
        &second,
        peer,
        200,
        &[("sip-etag", "e-1"), ("expires", "3600")],
    );
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::StatusPublished(_))),
        Event::StatusPublished(Ok(PresenceStatus::Away))
    );

    // A new status names the one it replaces.
    assert!(handle.send(UiCommand::Presence(Some(PresenceStatus::Busy))));
    let (change, _) = server.request("PUBLISH");
    assert_eq!(change.headers.get("sip-if-match"), Some("e-1"));
    assert!(String::from_utf8_lossy(&change.body).contains("<rpid:busy/>"));
    // An exchange that does not take statuses, like Asterisk.
    server.reply(&change, peer, 489, &[]);
    assert_eq!(
        wait_for(&handle, |event| matches!(event, Event::StatusPublished(_))),
        Event::StatusPublished(Err("489 Scripted".to_owned()))
    );

    // With statuses on, a watched colleague's status is asked for too.
    assert!(handle.send(UiCommand::Watch(vec!["4010".to_owned()])));
    let mut presence = None;
    for _ in 0..2 {
        let (subscribe, peer) = server.request("SUBSCRIBE");
        server.reply(&subscribe, peer, 200, &[("expires", "600")]);
        if subscribe.headers.get("event") == Some("presence") {
            assert_eq!(
                subscribe.headers.get("accept"),
                Some("application/pidf+xml")
            );
            presence = Some(subscribe);
        }
    }
    let subscribe = presence.expect("a presence subscription");
    let pidf = "<?xml version=\"1.0\"?><presence xmlns=\"urn:ietf:params:xml:ns:pidf\" \
        entity=\"sip:4010@pbx\"><tuple id=\"a\"><status><basic>open</basic></status>\
        <note>Away</note></tuple></presence>";
    server.send(
        &notify(
            &subscribe,
            1,
            "active;expires=600",
            "application/pidf+xml",
            pidf,
        ),
        peer,
    );
    let Event::Status { number, presence } =
        wait_for(&handle, |event| matches!(event, Event::Status { .. }))
    else {
        unreachable!();
    };
    assert_eq!(number, "4010");
    assert!(presence.online);
    assert_eq!(presence.status, Some(PresenceStatus::Away));

    // Statuses off: nothing was published, so nothing is taken down, and
    // colleagues are watched for their calls only.
    assert!(handle.send(UiCommand::Presence(None)));
    let mut events = Vec::new();
    for _ in 0..3 {
        let (request, peer) = server.request("SUBSCRIBE");
        server.reply(&request, peer, 200, &[("expires", "600")]);
        events.push(request.headers.get("event").unwrap_or("").to_owned());
        if events.len() == 3 {
            break;
        }
    }
    // The two old subscriptions end (dialog, presence), one new for dialog.
    events.sort();
    assert_eq!(events, ["dialog", "dialog", "presence"]);
}
