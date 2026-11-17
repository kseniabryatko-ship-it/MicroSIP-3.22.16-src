//! Encrypted audio (SRTP) and STUN in whole calls, without sound devices:
//! two phones on the loopback interface each send a tone and measure the
//! tone they hear, with every combination of the encryption setting.

use microsip_core::call_media::{NullMedia, ToneMedia, ToneReport};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::codec::Codec;
use microsip_core::rtp::session::{RtpSession, RtpSessionConfig};
use microsip_core::srtp::{MasterKey, MediaEncryption, SrtpKeys, Suite};
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

struct Phone {
    handle: CallHandle,
    report: Arc<Mutex<ToneReport>>,
}

fn phone(tone_hz: f32, encryption: MediaEncryption) -> Phone {
    let report = Arc::new(Mutex::new(ToneReport::default()));
    let for_media = Arc::clone(&report);
    let handle = spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        Timers::default(),
        move || ToneMedia::new(tone_hz, for_media),
    )
    .expect("executor should start");
    assert!(handle.send(UiCommand::Encryption(encryption)));
    Phone { handle, report }
}

/// Waits for `expected`, failing on any reported error or on timeout.
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

/// A call between phones with these settings. `Some(encrypted)` when it
/// connected and both voices arrived; `None` when it was refused.
fn call(caller: MediaEncryption, called: MediaEncryption) -> Option<bool> {
    let a = phone(440.0, caller);
    let b = phone(1_000.0, called);
    // The settings must be in place before the call starts.
    std::thread::sleep(Duration::from_millis(100));
    assert!(a.handle.send(UiCommand::Dial(b.handle.local_address())));

    // Either the called phone rings or the caller is told it was refused.
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the call went nowhere");
        if let Some(Event::Incoming { .. }) = b.handle.wait_event(Duration::from_millis(20)) {
            break;
        }
        if let Some(Event::Ended(reason)) = a.handle.try_event() {
            assert_eq!(reason, EndReason::Rejected(488));
            return None;
        }
    }
    assert!(b.handle.send(UiCommand::Answer));
    let encrypted = loop {
        assert!(Instant::now() < deadline, "the audio did not start");
        if let Some(Event::Encrypted(encrypted)) = a.handle.wait_event(Duration::from_millis(50)) {
            break encrypted;
        }
    };
    wait_for(&b.handle, Event::Encrypted(encrypted));
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(a.handle.send(UiCommand::Hangup));
    wait_for(&a.handle, Event::Ended(EndReason::Completed));
    wait_for(&b.handle, Event::Ended(EndReason::Completed));
    for (report, hz) in [(&a.report, 1_000.0), (&b.report, 440.0)] {
        let heard = *report.lock().unwrap();
        assert!(heard.seconds > 0.8, "only {} s heard", heard.seconds);
        assert!((heard.frequency - hz).abs() < 15.0, "{heard:?}");
        assert!((0.3..=0.5).contains(&heard.level), "{heard:?}");
    }
    Some(encrypted)
}

#[test]
fn calls_are_encrypted_whenever_both_sides_allow_it() {
    use MediaEncryption::{Off, Optional, Required};
    // (caller, called, outcome)
    for (caller, called, outcome) in [
        (Required, Required, Some(true)),
        (Required, Optional, Some(true)),
        (Optional, Required, Some(true)),
        (Optional, Optional, Some(true)),
        // One side cannot or will not: the call is plain where that is
        // allowed and refused where encryption is a must.
        (Optional, Off, Some(false)),
        (Off, Optional, Some(false)),
        (Off, Off, Some(false)),
        (Required, Off, None),
        (Off, Required, None),
    ] {
        assert_eq!(
            call(caller, called),
            outcome,
            "{caller:?} calling {called:?}"
        );
    }
}

fn keys(local: &MasterKey, remote: &MasterKey) -> SrtpKeys {
    SrtpKeys {
        suite: Suite::AesCm128HmacSha1_80,
        local: local.clone(),
        remote: remote.clone(),
    }
}

/// A session that reports the level of every block it hears.
fn listener(keys: Option<SrtpKeys>) -> (RtpSession, mpsc::Receiver<f32>) {
    let (heard_sender, heard) = mpsc::channel::<f32>();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let session = RtpSession::from_socket_with_sink(
        socket,
        RtpSessionConfig::new("127.0.0.1:9".parse().unwrap(), 8_000)
            .with_expected_source(None)
            .with_srtp(keys),
        0xBBBB_0001,
        Some(Box::new(move |samples| {
            let _ = heard_sender.send(samples[0]);
        })),
    )
    .unwrap();
    (session, heard)
}

#[test]
fn only_the_holder_of_the_keys_hears_the_voice() {
    let (ours, theirs) = (
        MasterKey::generate().unwrap(),
        MasterKey::generate().unwrap(),
    );
    let stranger = MasterKey::generate().unwrap();

    let (mut friend, friend_hears) = listener(Some(keys(&theirs, &ours)));
    let (mut eavesdropper, eavesdropper_hears) = listener(Some(keys(&theirs, &stranger)));
    // What travels on the wire, as anybody in between sees it.
    let wire = UdpSocket::bind("127.0.0.1:0").unwrap();
    wire.set_read_timeout(Some(WAIT)).unwrap();

    let targets = [
        friend.local_address().unwrap(),
        eavesdropper.local_address().unwrap(),
        wire.local_addr().unwrap(),
    ];
    let mut senders = Vec::new();
    for target in targets {
        let sender = RtpSession::bind(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            RtpSessionConfig::new(target, 8_000).with_srtp(Some(keys(&ours, &theirs))),
            0xBBBB_0002,
        )
        .unwrap();
        sender.push_pcm(&[0.5; 160]).unwrap();
        senders.push(sender);
    }

    let level = friend_hears
        .recv_timeout(WAIT)
        .expect("the other side of the call hears");
    assert!((level - 0.5).abs() < 0.02, "heard {level}");
    // With the wrong key nothing is played, not even noise.
    assert!(
        eavesdropper_hears
            .recv_timeout(Duration::from_millis(300))
            .is_err()
    );

    // On the wire: an RTP header, then bytes that are not the voice, then
    // ten bytes of signature.
    let mut buffer = [0_u8; 2_048];
    let (length, _) = wire.recv_from(&mut buffer).unwrap();
    assert_eq!(length, 12 + 160 + 10);
    assert_eq!(buffer[0], 0x80);
    let plain = Codec::Pcmu.encode(&[0.5; 160]);
    let same = buffer[12..172]
        .iter()
        .zip(&plain)
        .filter(|(sent, voice)| sent == voice)
        .count();
    assert!(same < 16, "{same} of 160 bytes are the plain voice");

    for mut session in senders {
        session.shutdown().unwrap();
    }
    for session in [&mut friend, &mut eavesdropper] {
        session.shutdown().unwrap();
    }
}

/// A STUN server on the loopback interface that tells everyone they are
/// seen as `public`.
fn stun_server(public: std::net::SocketAddrV4) -> SocketAddr {
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 64];
        server.set_read_timeout(Some(WAIT)).unwrap();
        while let Ok((length, source)) = server.recv_from(&mut buffer) {
            assert_eq!(length, 20);
            let mut message = vec![0x01, 0x01, 0x00, 0x0C];
            message.extend_from_slice(&buffer[4..20]);
            message.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
            message.extend_from_slice(&(public.port() ^ 0x2112).to_be_bytes());
            for (byte, mask) in public.ip().octets().iter().zip([0x21, 0x12, 0xA4, 0x42]) {
                message.push(byte ^ mask);
            }
            server.send_to(&message, source).unwrap();
        }
    });
    address
}

/// The SDP a phone offers when it dials, after it was given `setup`.
fn offer_after(setup: Vec<UiCommand>) -> String {
    let handle = spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        // Nobody answers these calls; they are given up quickly.
        Timers {
            t1: Duration::from_millis(30),
            t2: Duration::from_millis(100),
            give_up: Duration::from_millis(400),
        },
        NullMedia::default,
    )
    .unwrap();
    for command in setup {
        assert!(handle.send(command));
    }
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(WAIT)).unwrap();
    assert!(handle.send(UiCommand::Dial(peer.local_addr().unwrap())));
    let mut buffer = [0_u8; 8_192];
    let (length, _) = peer.recv_from(&mut buffer).expect("an INVITE");
    let text = String::from_utf8_lossy(&buffer[..length]).into_owned();
    assert!(text.starts_with("INVITE "), "{text}");
    text
}

#[test]
fn the_offer_carries_the_address_the_stun_server_reports() {
    // Without STUN the audio is announced at the phone's own address.
    let plain = offer_after(Vec::new());
    assert!(plain.contains("c=IN IP4 127.0.0.1\r\n"), "{plain}");

    let stun = stun_server("203.0.113.9:40000".parse().unwrap());
    let public = offer_after(vec![UiCommand::Stun(Some(stun.to_string()))]);
    assert!(public.contains("c=IN IP4 203.0.113.9\r\n"), "{public}");
    assert!(public.contains("m=audio 40000 RTP/AVP "), "{public}");

    // Set and switched off again: back to the own address.
    let plain = offer_after(vec![
        UiCommand::Stun(Some(stun.to_string())),
        UiCommand::Stun(None),
    ]);
    assert!(plain.contains("c=IN IP4 127.0.0.1\r\n"), "{plain}");

    // A server that does not answer costs under a second and the call
    // goes out from the local address.
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let started = Instant::now();
    let fallback = offer_after(vec![UiCommand::Stun(Some(
        silent.local_addr().unwrap().to_string(),
    ))]);
    assert!(fallback.contains("c=IN IP4 127.0.0.1\r\n"), "{fallback}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_required_call_is_offered_as_savp_with_a_key_and_an_optional_one_as_avp() {
    let required = offer_after(vec![UiCommand::Encryption(MediaEncryption::Required)]);
    assert!(required.contains(" RTP/SAVP "), "{required}");
    assert!(
        required.contains("a=crypto:1 AES_CM_128_HMAC_SHA1_80 inline:"),
        "{required}"
    );

    let optional = offer_after(vec![UiCommand::Encryption(MediaEncryption::Optional)]);
    assert!(optional.contains(" RTP/AVP "), "{optional}");
    assert!(optional.contains("a=crypto:1 "), "{optional}");
    // Every call has a key of its own.
    let key = |sdp: &str| sdp.split("inline:").nth(1).unwrap()[..40].to_owned();
    assert_ne!(key(&required), key(&optional));

    let off = offer_after(vec![UiCommand::Encryption(MediaEncryption::Off)]);
    assert!(
        off.contains(" RTP/AVP ") && !off.contains("a=crypto"),
        "{off}"
    );
}
