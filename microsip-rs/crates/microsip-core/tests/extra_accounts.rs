//! Several accounts at once: an extra account registers from the same
//! address as the main one, its calls ring here and are told apart, and it
//! calls out with its own name and password. A scripted server on a raw
//! loopback socket plays the exchange.

use microsip_core::call_media::NullMedia;
use microsip_core::call_runner::{
    CallHandle, Event, Timers, UiCommand, account_key, spawn_with_timers,
};
use microsip_core::registration::{Account, RegistrationState};
use microsip_core::sip::message::{Message, Request, ResponseBuilder};
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

fn contact_host(request: &Request) -> String {
    let contact = request.headers.get("contact").unwrap();
    contact
        .trim_matches(['<', '>'])
        .split('@')
        .nth(1)
        .unwrap()
        .split(['>', ';'])
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn an_extra_account_registers_rings_and_calls_with_its_own_name() {
    let server = Server::new();
    let handle = phone();
    let (main_subscribe, peer) = server.register(&handle);
    let extra = Account::new(server.address().to_string(), "4011", "other-secret");
    let key = account_key(&extra);
    assert_eq!(key, format!("4011@{}", server.address()));

    // It registers from the same address, with its own name and password.
    assert!(handle.send(UiCommand::ExtraAccounts(vec![extra.clone()])));
    let (first, extra_peer) = server.request("REGISTER");
    assert_eq!(extra_peer, peer);
    assert!(first.headers.get("from").unwrap().contains("sip:4011@"));
    assert_eq!(contact_host(&first), contact_host(&main_subscribe));
    server.reply(&first, peer, 401, &[("www-authenticate", CHALLENGE)]);
    let (second, _) = server.request("REGISTER");
    assert!(
        second
            .headers
            .get("authorization")
            .unwrap()
            .contains("username=\"4011\"")
    );
    server.reply(&second, peer, 200, &[("expires", "3600")]);
    let event = wait_for(&handle, |event| {
        matches!(
            event,
            Event::AccountRegistration {
                state: RegistrationState::Registered { .. },
                ..
            }
        )
    });
    let Event::AccountRegistration { account, .. } = event else {
        unreachable!();
    };
    assert_eq!(account, key);

    // A call through it goes out as 4011 and answers the challenge as 4011.
    assert!(handle.send(UiCommand::DialFrom {
        account: key.clone(),
        target: "4010".to_owned(),
    }));
    let (invite, _) = server.request("INVITE");
    assert!(invite.headers.get("from").unwrap().contains("sip:4011@"));
    assert!(invite.headers.get("contact").unwrap().contains("sip:4011@"));
    server.reply(&invite, peer, 407, &[("proxy-authenticate", CHALLENGE)]);
    let (again, _) = server.request("INVITE");
    assert!(
        again
            .headers
            .get("proxy-authorization")
            .unwrap()
            .contains("username=\"4011\"")
    );
    server.reply(&again, peer, 486, &[]);
    wait_for(&handle, |event| matches!(event, Event::Ended(_)));

    // An unknown extra account cannot call.
    assert!(handle.send(UiCommand::DialFrom {
        account: "nobody@nowhere".to_owned(),
        target: "4010".to_owned(),
    }));
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "no error was reported");
        if let Some(Event::Error(text)) = handle.wait_event(Duration::from_millis(50)) {
            assert!(text.contains("not registered"), "{text}");
            break;
        }
    }

    // Taking it off tells the server.
    assert!(handle.send(UiCommand::ExtraAccounts(Vec::new())));
    let (bye, _) = server.request("REGISTER");
    assert!(bye.headers.get("from").unwrap().contains("sip:4011@"));
    assert_eq!(bye.headers.get("expires"), Some("0"));
    wait_for(&handle, |event| {
        *event
            == Event::AccountRegistration {
                account: key.clone(),
                state: RegistrationState::Unregistered,
            }
    });
}

#[test]
fn extra_accounts_over_tcp_are_refused_with_a_reason() {
    let handle = phone();
    let mut extra = Account::new("127.0.0.1:9", "4012", "x");
    extra.transport = microsip_core::stream::SipTransport::Tcp;
    assert!(handle.send(UiCommand::ExtraAccounts(vec![extra])));
    let event = wait_for(&handle, |event| {
        matches!(event, Event::AccountRegistration { .. })
    });
    let Event::AccountRegistration {
        state: RegistrationState::Failed(reason),
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert!(reason.contains("UDP"), "{reason}");
}
