use microsip_core::call_engine::{Action, CallEngine, Command, Input, State};
use microsip_core::dialog::{DialogConfig, EstablishedDialog};
use microsip_core::invite_control::{build_cancel, build_failure_ack};
use microsip_core::sip::message::{InviteRequest, Message, Request, Response, ResponseBuilder};
use microsip_core::transaction_headers::response_matches_request;
use std::error::Error;
use std::io;
use std::net::UdpSocket;
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Link {
    caller: UdpSocket,
    callee: UdpSocket,
}

impl Link {
    fn new() -> TestResult<Self> {
        let caller = UdpSocket::bind("127.0.0.1:0")?;
        let callee = UdpSocket::bind("127.0.0.1:0")?;
        caller.set_read_timeout(Some(Duration::from_secs(2)))?;
        callee.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self { caller, callee })
    }

    fn to_callee(&self, message: Message) -> TestResult<Message> {
        transfer(&self.caller, &self.callee, message)
    }

    fn to_caller(&self, message: Message) -> TestResult<Message> {
        transfer(&self.callee, &self.caller, message)
    }

    fn invite(&self) -> TestResult<Request> {
        let caller = self.caller.local_addr()?;
        let callee = self.callee.local_addr()?;
        Ok(InviteRequest::new(
            format!("sip:bob@{callee}"),
            "sip:alice@local.test",
            "sip:bob@local.test",
            format!("sip:alice@{caller}"),
            caller.to_string(),
            "integration-call@local.test",
            "z9hG4bK-integration-invite",
            "alice-tag",
        )
        .build()?)
    }
}

fn transfer(from: &UdpSocket, to: &UdpSocket, message: Message) -> TestResult<Message> {
    let bytes = message.serialize()?;
    let sent = from.send_to(&bytes, to.local_addr()?)?;
    assert_eq!(sent, bytes.len());

    let mut buffer = [0_u8; 65_535];
    let (length, source) = to.recv_from(&mut buffer)?;
    assert_eq!(source, from.local_addr()?);
    Ok(Message::parse(std::str::from_utf8(&buffer[..length])?)?)
}

fn required<'a>(request: &'a Request, name: &str) -> TestResult<&'a str> {
    request
        .headers
        .get(name)
        .ok_or_else(|| io::Error::other(format!("Missing request header: {name}")).into())
}

fn as_request(message: Message) -> TestResult<Request> {
    match message {
        Message::Request(request) => Ok(request),
        Message::Response(_) => Err(io::Error::other("Expected request").into()),
    }
}

fn as_response(message: Message) -> TestResult<Response> {
    match message {
        Message::Response(response) => Ok(response),
        Message::Request(_) => Err(io::Error::other("Expected response").into()),
    }
}

fn response(request: &Request, status: u16, contact: &str) -> TestResult<Response> {
    let reason = match status {
        180 => "Ringing",
        200 => "OK",
        486 => "Busy Here",
        487 => "Request Terminated",
        _ => "Test",
    };
    let original_to = required(request, "to")?;
    let to = if original_to.contains(";tag=") {
        original_to.to_owned()
    } else {
        format!("{original_to};tag=bob-tag")
    };

    Ok(ResponseBuilder::new(status, reason)
        .header("via", required(request, "via")?)?
        .header("from", required(request, "from")?)?
        .header("to", to)?
        .header("call-id", required(request, "call-id")?)?
        .header("cseq", required(request, "cseq")?)?
        .header("contact", format!("<{contact}>"))?
        .build()?)
}

fn assert_response_matches(request: &Request, response: &Response) -> TestResult {
    assert!(response_matches_request(
        &Message::Request(request.clone()),
        &Message::Response(response.clone()),
    )?);
    Ok(())
}

fn dialog(link: &Link, invite: &Request, success: &Response) -> TestResult<EstablishedDialog> {
    let contact = success
        .headers
        .get("contact")
        .ok_or_else(|| io::Error::other("Missing response Contact"))?;
    let remote_target = contact
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or_else(|| io::Error::other("Invalid test Contact"))?;
    let to = success
        .headers
        .get("to")
        .ok_or_else(|| io::Error::other("Missing response To"))?;

    Ok(EstablishedDialog::new(DialogConfig {
        remote_target: remote_target.to_owned(),
        local_from: required(invite, "from")?.to_owned(),
        remote_to: to.to_owned(),
        call_id: required(invite, "call-id")?.to_owned(),
        local_sent_by: link.caller.local_addr()?.to_string(),
        local_contact: format!("<sip:alice@{}>", link.caller.local_addr()?),
        invite_cseq: 1,
        invite_branch: "z9hG4bK-integration-invite".to_owned(),
    })?)
}

fn start(link: &Link, engine: &mut CallEngine) -> TestResult<(Request, Request)> {
    assert_eq!(
        engine.handle(Input::Command(Command::Dial))?,
        vec![Action::SendInvite]
    );
    let original = link.invite()?;
    let received = as_request(link.to_callee(Message::Request(original.clone()))?)?;
    assert_eq!(received, original);
    Ok((original, received))
}

#[test]
fn udp_success_ack_and_bye_complete_both_engines() -> TestResult {
    let link = Link::new()?;
    let mut caller = CallEngine::new();
    let mut callee = CallEngine::new();
    let (original, incoming) = start(&link, &mut caller)?;
    assert_eq!(
        callee.handle(Input::IncomingInvite)?,
        vec![Action::SendRinging]
    );

    let target = format!("sip:bob@{}", link.callee.local_addr()?);
    let ringing =
        as_response(link.to_caller(Message::Response(response(&incoming, 180, &target)?))?)?;
    assert_response_matches(&original, &ringing)?;
    assert!(
        caller
            .handle(Input::InviteResponse(ringing.status_code))?
            .is_empty()
    );

    assert_eq!(
        callee.handle(Input::Command(Command::Answer))?,
        vec![Action::SendInviteSuccess]
    );
    let success =
        as_response(link.to_caller(Message::Response(response(&incoming, 200, &target)?))?)?;
    assert_response_matches(&original, &success)?;
    assert_eq!(
        caller.handle(Input::InviteResponse(success.status_code))?,
        vec![
            Action::SendSuccessAck,
            Action::StartMedia,
            Action::ReportEstablished,
        ]
    );

    let mut dialog = dialog(&link, &original, &success)?;
    let ack = as_request(link.to_callee(Message::Request(
        dialog.build_success_ack("z9hG4bK-integration-success-ack")?,
    ))?)?;
    assert_eq!(required(&ack, "cseq")?, "1 ACK");
    assert_ne!(required(&ack, "via")?, required(&original, "via")?);
    assert_eq!(
        callee.handle(Input::IncomingAck)?,
        vec![Action::StartMedia, Action::ReportEstablished]
    );

    assert_eq!(
        caller.handle(Input::Command(Command::Hangup))?,
        vec![Action::SendBye, Action::StopMedia]
    );
    let outgoing_bye = dialog.build_bye("z9hG4bK-integration-bye")?;
    let bye = as_request(link.to_callee(Message::Request(outgoing_bye.clone()))?)?;
    assert_eq!(required(&bye, "cseq")?, "2 BYE");
    assert_eq!(
        callee.handle(Input::IncomingBye)?,
        vec![
            Action::SendByeSuccess,
            Action::StopMedia,
            Action::ReportEnded,
        ]
    );
    let finished = as_response(link.to_caller(Message::Response(response(&bye, 200, &target)?))?)?;
    assert_response_matches(&outgoing_bye, &finished)?;
    assert_eq!(
        caller.handle(Input::ByeResponse(finished.status_code))?,
        vec![Action::ReportEnded]
    );
    dialog.finish_local_bye()?;
    assert_eq!(caller.state(), State::Idle);
    assert_eq!(callee.state(), State::Idle);
    Ok(())
}

#[test]
fn udp_rejection_produces_failure_ack_with_original_branch() -> TestResult {
    let link = Link::new()?;
    let mut caller = CallEngine::new();
    let mut callee = CallEngine::new();
    let (original, incoming) = start(&link, &mut caller)?;
    callee.handle(Input::IncomingInvite)?;
    assert_eq!(
        callee.handle(Input::Command(Command::Reject))?,
        vec![Action::SendInviteFailure(486), Action::ReportEnded,]
    );
    let reply = as_response(link.to_caller(Message::Response(response(
        &incoming,
        486,
        "sip:bob@local.test",
    )?))?)?;
    assert_response_matches(&original, &reply)?;
    assert_eq!(
        caller.handle(Input::InviteResponse(reply.status_code))?,
        vec![Action::SendFailureAck, Action::ReportEnded,]
    );
    let ack = as_request(link.to_callee(Message::Request(build_failure_ack(&original, &reply)?))?)?;
    assert_eq!(required(&ack, "cseq")?, "1 ACK");
    assert_eq!(required(&ack, "via")?, required(&original, "via")?);
    assert_eq!(ack.headers.get("to"), reply.headers.get("to"));
    assert_eq!(caller.state(), State::Idle);
    assert_eq!(callee.state(), State::Idle);
    // Failure ACK belongs to the retained INVITE transaction, not a new call.
    assert_eq!(
        caller.handle(Input::Command(Command::Dial))?,
        vec![Action::SendInvite]
    );
    Ok(())
}

#[test]
fn udp_cancel_has_its_own_response_and_invite_failure_ack() -> TestResult {
    let link = Link::new()?;
    let mut caller = CallEngine::new();
    let mut callee = CallEngine::new();
    let (original, incoming) = start(&link, &mut caller)?;
    callee.handle(Input::IncomingInvite)?;
    let ringing = as_response(link.to_caller(Message::Response(response(
        &incoming,
        180,
        "sip:bob@local.test",
    )?))?)?;
    assert_response_matches(&original, &ringing)?;
    caller.handle(Input::InviteResponse(ringing.status_code))?;
    assert_eq!(
        caller.handle(Input::Command(Command::Hangup))?,
        vec![Action::SendCancel]
    );

    let outgoing_cancel = build_cancel(&original)?;
    let cancel = as_request(link.to_callee(Message::Request(outgoing_cancel.clone()))?)?;
    assert_eq!(required(&cancel, "cseq")?, "1 CANCEL");
    assert_eq!(required(&cancel, "via")?, required(&original, "via")?);
    assert_eq!(
        callee.handle(Input::IncomingCancel)?,
        vec![
            Action::SendCancelSuccess,
            Action::SendInviteFailure(487),
            Action::ReportEnded,
        ]
    );

    let cancel_ok = as_response(link.to_caller(Message::Response(response(
        &cancel,
        200,
        "sip:bob@local.test",
    )?))?)?;
    assert_response_matches(&outgoing_cancel, &cancel_ok)?;
    // 200 to CANCEL does not finish the INVITE transaction.
    assert_eq!(caller.state(), State::Outgoing);

    let terminated = as_response(link.to_caller(Message::Response(response(
        &incoming,
        487,
        "sip:bob@local.test",
    )?))?)?;
    assert_response_matches(&original, &terminated)?;
    assert_eq!(
        caller.handle(Input::InviteResponse(terminated.status_code))?,
        vec![Action::SendFailureAck, Action::ReportEnded,]
    );
    let ack =
        as_request(link.to_callee(Message::Request(build_failure_ack(&original, &terminated)?))?)?;
    assert_eq!(required(&ack, "cseq")?, "1 ACK");
    assert_eq!(caller.state(), State::Idle);
    assert_eq!(callee.state(), State::Idle);
    Ok(())
}

#[test]
fn udp_success_winning_cancel_race_is_acked_then_ended_with_bye() -> TestResult {
    let link = Link::new()?;
    let mut caller = CallEngine::new();
    let mut callee = CallEngine::new();
    let (original, incoming) = start(&link, &mut caller)?;
    callee.handle(Input::IncomingInvite)?;
    caller.handle(Input::InviteResponse(180))?;
    assert_eq!(
        caller.handle(Input::Command(Command::Hangup))?,
        vec![Action::SendCancel]
    );
    let cancel = as_request(link.to_callee(Message::Request(build_cancel(&original)?))?)?;

    callee.handle(Input::Command(Command::Answer))?;
    assert_eq!(
        callee.handle(Input::IncomingCancel)?,
        vec![Action::SendCancelSuccess]
    );
    let target = format!("sip:bob@{}", link.callee.local_addr()?);
    let cancel_ok =
        as_response(link.to_caller(Message::Response(response(&cancel, 200, &target)?))?)?;
    assert_response_matches(&cancel, &cancel_ok)?;
    let success =
        as_response(link.to_caller(Message::Response(response(&incoming, 200, &target)?))?)?;
    assert_response_matches(&original, &success)?;
    assert_eq!(
        caller.handle(Input::InviteResponse(success.status_code))?,
        vec![Action::SendSuccessAck, Action::SendBye,]
    );

    let mut dialog = dialog(&link, &original, &success)?;
    let ack = as_request(link.to_callee(Message::Request(
        dialog.build_success_ack("z9hG4bK-race-ack")?,
    ))?)?;
    assert_eq!(required(&ack, "cseq")?, "1 ACK");
    callee.handle(Input::IncomingAck)?;
    let outgoing_bye = dialog.build_bye("z9hG4bK-race-bye")?;
    let bye = as_request(link.to_callee(Message::Request(outgoing_bye.clone()))?)?;
    callee.handle(Input::IncomingBye)?;
    let finished = as_response(link.to_caller(Message::Response(response(&bye, 200, &target)?))?)?;
    assert_response_matches(&outgoing_bye, &finished)?;
    caller.handle(Input::ByeResponse(finished.status_code))?;
    assert_eq!(caller.state(), State::Idle);
    assert_eq!(callee.state(), State::Idle);
    Ok(())
}
