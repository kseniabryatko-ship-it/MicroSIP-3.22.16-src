//! Keypad digits as SIP INFO between two phones on the loopback interface:
//! each digit is one INFO request inside the call, sent one after another,
//! and the other phone confirms each.

use microsip_core::call_media::{ToneMedia, ToneReport};
use microsip_core::call_runner::{
    CallHandle, EndReason, Event, Timers, UiCommand, spawn_with_timers,
};
use microsip_core::rtp::dtmf::DtmfMethod;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

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
fn digits_go_one_by_one_as_sip_info_and_are_confirmed() {
    let a = phone(440.0);
    let b = phone(1_000.0);
    assert!(a.send(UiCommand::DtmfMethod(DtmfMethod::SipInfo)));
    assert!(a.send(UiCommand::Trace(true)));
    assert!(a.send(UiCommand::Dial(b.local_address())));
    wait_for(&b, |event| matches!(event, Event::Incoming { .. }));
    assert!(b.send(UiCommand::Answer));
    wait_for(&a, |event| *event == Event::Established);

    for digit in ['1', '#', '9'] {
        assert!(a.send(UiCommand::Dtmf(digit)));
    }
    // Every INFO carries one digit, and every one is answered with 200.
    let mut sent = Vec::new();
    let mut confirmed = 0;
    let deadline = Instant::now() + WAIT;
    while (sent.len() < 3 || confirmed < 3) && Instant::now() < deadline {
        match a.wait_event(Duration::from_millis(50)) {
            Some(Event::Trace { outgoing, text, .. }) => {
                let lower = text.to_ascii_lowercase();
                if outgoing && text.starts_with("INFO ") {
                    assert!(lower.contains("content-type: application/dtmf-relay"));
                    let signal = text
                        .lines()
                        .find_map(|line| line.strip_prefix("Signal="))
                        .expect("a Signal line")
                        .trim()
                        .to_owned();
                    sent.push(signal);
                } else if !outgoing
                    && text.starts_with("SIP/2.0 200")
                    && lower.contains(" info\r\n")
                {
                    confirmed += 1;
                }
            }
            Some(Event::Error(message)) => panic!("executor reported an error: {message}"),
            _ => {}
        }
    }
    assert_eq!(sent, ["1", "#", "9"]);
    assert_eq!(confirmed, 3);

    assert!(a.send(UiCommand::Hangup));
    wait_for(&a, |event| *event == Event::Ended(EndReason::Completed));
    wait_for(&b, |event| *event == Event::Ended(EndReason::Completed));
}

#[test]
fn sip_info_digits_need_a_connected_call() {
    let a = phone(440.0);
    assert!(a.send(UiCommand::DtmfMethod(DtmfMethod::SipInfo)));
    assert!(a.send(UiCommand::Dtmf('5')));
    let Event::Error(message) = wait_for(&a, |event| matches!(event, Event::Error(_))) else {
        unreachable!();
    };
    assert!(message.contains("connected call"), "{message}");
}
