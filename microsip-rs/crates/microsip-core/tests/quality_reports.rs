//! RTCP reports between two phones on the loopback interface: each learns
//! from the other's reports how much of its own audio got lost on the way.

use microsip_core::call_media::{ToneMedia, ToneReport};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::rtp::session::{RtcpConfig, RtpSession, RtpSessionConfig};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

fn socket() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").unwrap()
}

/// Passes datagrams from `from` on to `to`, dropping every `drop_every`-th
/// one, until told to stop: a network that loses packets.
fn lossy_link(from: UdpSocket, to: SocketAddr, drop_every: u32, stop: Arc<AtomicBool>) {
    from.set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    thread::spawn(move || {
        let mut buffer = [0_u8; 2_048];
        let mut count = 0_u32;
        while !stop.load(Ordering::Acquire) {
            if let Ok((length, _)) = from.recv_from(&mut buffer) {
                count += 1;
                if !count.is_multiple_of(drop_every) {
                    let _ = from.send_to(&buffer[..length], to);
                }
            }
        }
    });
}

#[test]
fn the_sender_learns_its_loss_and_the_delay_from_the_other_sides_reports() {
    // B listens for audio on `b_rtp`; A sends it to a lossy link that
    // forwards to B. Their reports go straight between their RTCP sockets.
    let (a_rtp, b_rtp, link) = (socket(), socket(), socket());
    let (a_rtcp, b_rtcp) = (socket(), socket());
    let link_address = link.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    lossy_link(link, b_rtp.local_addr().unwrap(), 10, Arc::clone(&stop));
    let interval = Duration::from_millis(300);

    let b_rtcp_address = b_rtcp.local_addr().unwrap();
    let a_rtcp_address = a_rtcp.local_addr().unwrap();
    let mut a = RtpSession::from_socket(
        a_rtp,
        RtpSessionConfig::new(link_address, 8_000).with_rtcp(Some(RtcpConfig {
            socket: Arc::new(a_rtcp),
            remote: b_rtcp_address,
            interval,
        })),
        0xA0A0_0001,
    )
    .unwrap();
    let mut b = RtpSession::from_socket(
        b_rtp,
        RtpSessionConfig::new("127.0.0.1:9".parse().unwrap(), 8_000)
            .with_expected_source(None)
            .with_rtcp(Some(RtcpConfig {
                socket: Arc::new(b_rtcp),
                remote: a_rtcp_address,
                interval,
            })),
        0xB0B0_0002,
    )
    .unwrap();

    // Two seconds of audio, 20 ms at a time, at the pace of a call.
    let started = Instant::now();
    for block in 0..100_u32 {
        a.push_pcm(&[0.2; 160]).unwrap();
        let due = started + Duration::from_millis(u64::from(block + 1) * 20);
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    // Reports keep coming after the audio; the last one sums it all.
    thread::sleep(interval * 3);

    let quality = a.remote_quality().expect("B reported");
    assert!(
        (8..=12).contains(&quality.lost_total),
        "B lost {} of 100 packets",
        quality.lost_total
    );
    let round_trip = quality.round_trip_ms.expect("a round trip was measured");
    assert!(
        round_trip < 100.0,
        "round trip {round_trip} ms on one computer"
    );
    assert!(quality.jitter_ms < 30.0, "jitter {} ms", quality.jitter_ms);
    // B sent no audio, so A has nothing to say about it.
    assert!(b.remote_quality().is_none());

    stop.store(true, Ordering::Release);
    a.shutdown().unwrap();
    b.shutdown().unwrap();
}

#[test]
fn a_session_without_rtcp_reports_nothing() {
    let (a_rtp, b_rtp) = (socket(), socket());
    let target = b_rtp.local_addr().unwrap();
    let mut a = RtpSession::from_socket(a_rtp, RtpSessionConfig::new(target, 8_000), 1).unwrap();
    a.push_pcm(&[0.1; 160]).unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(a.remote_quality().is_none());
    a.shutdown().unwrap();
}

fn phone(tone_hz: f32) -> CallHandle {
    let report = Arc::new(Mutex::new(ToneReport::default()));
    spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        Timers::default(),
        move || ToneMedia::new(tone_hz, Arc::clone(&report)),
    )
    .expect("executor should start")
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

#[test]
fn after_a_call_the_quality_includes_the_other_sides_view() {
    let a = phone(440.0);
    let b = phone(1_000.0);
    assert!(a.send(UiCommand::Dial(b.local_address())));
    wait_for(&b, |event| matches!(event, Event::Incoming { .. }));
    assert!(b.send(UiCommand::Answer));
    wait_for(&a, |event| *event == Event::Established);
    // The first reports go out a second into the call.
    thread::sleep(Duration::from_millis(2_000));
    assert!(a.send(UiCommand::Hangup));
    let quality = wait_for(&a, |event| matches!(event, Event::AudioQuality(_)));
    let Event::AudioQuality(quality) = quality else {
        unreachable!();
    };
    let remote = quality.remote.expect("the other phone reported");
    assert_eq!(remote.lost_percent, 0);
    assert!(remote.lost_total <= 1, "{remote:?}");
    wait_for(&a, |event| *event == Event::Ended(EndReason::Completed));
    wait_for(&b, |event| *event == Event::Ended(EndReason::Completed));
}
