//! Whole calls with every codec, without sound devices: two phones on the
//! loopback interface each send a tone of their own pitch and measure the
//! tone they hear. The sound crosses the real encoders, RTP sockets,
//! packet ordering and decoders.

use microsip_core::call_media::{ToneMedia, ToneReport};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::codec::Codec;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

struct Phone {
    handle: CallHandle,
    report: Arc<Mutex<ToneReport>>,
}

fn phone(tone_hz: f32, codecs: &[Codec]) -> Phone {
    let report = Arc::new(Mutex::new(ToneReport::default()));
    let for_media = Arc::clone(&report);
    let handle = spawn_with_timers(
        "127.0.0.1:0".parse().unwrap(),
        "phone",
        Timers::default(),
        move || ToneMedia::new(tone_hz, for_media),
    )
    .expect("executor should start");
    assert!(handle.send(UiCommand::Codecs(codecs.to_vec())));
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

/// Calls from a phone that allows `caller` to one that allows `called`,
/// lets them talk for a moment and returns what each heard.
fn talk(caller: &[Codec], called: &[Codec], expected: Codec) -> (ToneReport, ToneReport) {
    let a = phone(440.0, caller);
    let b = phone(1_000.0, called);
    // The codec lists must be in place before the call starts.
    std::thread::sleep(Duration::from_millis(100));
    assert!(a.handle.send(UiCommand::Dial(b.handle.local_address())));
    wait_for(
        &b.handle,
        Event::Incoming {
            peer: a.handle.local_address(),
        },
    );
    assert!(b.handle.send(UiCommand::Answer));
    // Both sides report the codec their audio started with.
    wait_for(&a.handle, Event::Codec(expected));
    wait_for(&b.handle, Event::Codec(expected));
    std::thread::sleep(Duration::from_millis(1_500));
    assert!(a.handle.send(UiCommand::Hangup));
    wait_for(&a.handle, Event::Ended(EndReason::Completed));
    wait_for(&b.handle, Event::Ended(EndReason::Completed));
    let heard_by_a = *a.report.lock().unwrap();
    let heard_by_b = *b.report.lock().unwrap();
    (heard_by_a, heard_by_b)
}

fn assert_tone(report: ToneReport, hz: f32, what: &str) {
    assert!(report.seconds > 1.0, "{what}: only {} s", report.seconds);
    assert!(
        (report.frequency - hz).abs() < 15.0,
        "{what}: heard {} Hz instead of {hz}",
        report.frequency
    );
    assert!(
        (0.3..=0.5).contains(&report.level),
        "{what}: level {}",
        report.level
    );
}

#[test]
fn every_codec_carries_both_voices_of_a_call() {
    for codec in Codec::PREFERRED {
        let (heard_by_a, heard_by_b) = talk(&[codec], &[codec], codec);
        assert_tone(heard_by_a, 1_000.0, &format!("{codec} to the caller"));
        assert_tone(heard_by_b, 440.0, &format!("{codec} to the called"));
    }
}

#[test]
fn the_called_phones_order_decides_among_what_the_caller_offers() {
    // The caller offers everything; the called phone likes Opus best.
    let (heard_by_a, heard_by_b) = talk(
        &Codec::PREFERRED,
        &[Codec::Opus, Codec::G722, Codec::Pcmu],
        Codec::Opus,
    );
    assert_tone(heard_by_a, 1_000.0, "caller");
    assert_tone(heard_by_b, 440.0, "called");

    // The called phone has only one codec in common with the caller.
    let (heard_by_a, heard_by_b) = talk(
        &[Codec::Pcmu, Codec::G722],
        &[Codec::G729, Codec::Opus, Codec::G722],
        Codec::G722,
    );
    assert_tone(heard_by_a, 1_000.0, "caller");
    assert_tone(heard_by_b, 440.0, "called");
}

#[test]
fn phones_without_a_common_codec_do_not_connect() {
    let a = phone(440.0, &[Codec::Pcmu, Codec::Pcma]);
    let b = phone(1_000.0, &[Codec::Opus, Codec::G729]);
    std::thread::sleep(Duration::from_millis(100));
    assert!(a.handle.send(UiCommand::Dial(b.handle.local_address())));
    wait_for(&a.handle, Event::Ended(EndReason::Rejected(488)));
    assert_eq!(*a.report.lock().unwrap(), ToneReport::default());
}
