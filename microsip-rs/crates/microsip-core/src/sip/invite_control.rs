use crate::sip::message::{Headers, Method, Request, RequestBuilder, Response};
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteControlError(String);

impl fmt::Display for InviteControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for InviteControlError {}

type Result<T> = std::result::Result<T, InviteControlError>;

/// Builds CANCEL for an original INVITE.
/// The caller must decide whether cancellation is still appropriate.
pub fn build_cancel(invite: &Request) -> Result<Request> {
    let sequence = validate_invite(invite)?;
    build_related_request(
        invite,
        Method::Cancel,
        sequence,
        required(&invite.headers, "to")?,
    )
}

/// Builds transaction ACK for a final 300..699 INVITE response.
/// Not suitable for ACK to a successful 2xx response.
pub fn build_failure_ack(invite: &Request, response: &Response) -> Result<Request> {
    let sequence = validate_invite(invite)?;

    if !(300..=699).contains(&response.status_code) {
        return Err(problem("Failure ACK requires a 300..699 response"));
    }

    if response.version != invite.version {
        return Err(problem("SIP versions do not match"));
    }

    if required(&invite.headers, "call-id")? != required(&response.headers, "call-id")? {
        return Err(problem("Response call-id does not match INVITE"));
    }
    // Servers may rewrite the From address (Asterisk drops the port), so the
    // caller is identified by the From tag, as RFC 3261 does.
    let sent_tag = tag(required(&invite.headers, "from")?);
    if sent_tag.is_none() || sent_tag != tag(required(&response.headers, "from")?) {
        return Err(problem("Response from does not match INVITE"));
    }

    if parse_invite_cseq(required(&response.headers, "cseq")?)? != sequence {
        return Err(problem("Response CSeq does not match INVITE"));
    }

    let request_branch = top_branch(&invite.headers)?;
    let response_branch = top_branch(&response.headers)?;
    if request_branch != response_branch {
        return Err(problem("Response Via branch does not match INVITE"));
    }

    build_related_request(
        invite,
        Method::Ack,
        sequence,
        required(&response.headers, "to")?,
    )
}

fn build_related_request(
    invite: &Request,
    method: Method,
    sequence: u32,
    to: &str,
) -> Result<Request> {
    let cseq = format!("{sequence} {}", method.as_str());
    let max_forwards = invite.headers.get("max-forwards").unwrap_or("70");

    let mut builder = RequestBuilder::new(method, invite.target.clone())
        .version(invite.version)
        .header("via", required(&invite.headers, "via")?)
        .map_err(|error| problem(error.to_string()))?
        .header("from", required(&invite.headers, "from")?)
        .map_err(|error| problem(error.to_string()))?
        .header("to", to)
        .map_err(|error| problem(error.to_string()))?
        .header("call-id", required(&invite.headers, "call-id")?)
        .map_err(|error| problem(error.to_string()))?
        .header("cseq", cseq)
        .map_err(|error| problem(error.to_string()))?
        .header("max-forwards", max_forwards)
        .map_err(|error| problem(error.to_string()))?;

    for route in invite.headers.get_all("route") {
        builder = builder
            .append_header("route", route)
            .map_err(|error| problem(error.to_string()))?;
    }

    builder.build().map_err(|error| problem(error.to_string()))
}

fn validate_invite(invite: &Request) -> Result<u32> {
    if invite.method != Method::Invite {
        return Err(problem("Original request must be INVITE"));
    }
    // This module handles locally originated requests, not forwarded requests.
    required(&invite.headers, "via")?;
    top_branch(&invite.headers)?;
    required(&invite.headers, "from")?;
    required(&invite.headers, "to")?;
    required(&invite.headers, "call-id")?;
    parse_invite_cseq(required(&invite.headers, "cseq")?)
}

/// The `tag` parameter of a From or To value.
fn tag(value: &str) -> Option<&str> {
    let parameters = value.rsplit_once('>').map_or(value, |(_, rest)| rest);
    parameters
        .split(';')
        .filter_map(|part| part.split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("tag"))
        .map(|(_, tag)| tag.trim())
        .filter(|tag| !tag.is_empty())
}

fn required<'a>(headers: &'a Headers, name: &str) -> Result<&'a str> {
    let values = headers.get_all(name);
    if values.len() != 1 {
        return Err(problem(format!("Expected exactly one {name} header")));
    }
    let value = values[0].trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(problem(format!("Invalid {name} header")));
    }
    Ok(value)
}

fn parse_invite_cseq(value: &str) -> Result<u32> {
    let mut parts = value.split_whitespace();
    let number = parts.next().unwrap_or_default();
    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || parts.next() != Some("INVITE")
        || parts.next().is_some()
    {
        return Err(problem("Expected CSeq: <number> INVITE"));
    }
    let number = number
        .parse::<u32>()
        .map_err(|_| problem("Invalid CSeq number"))?;
    if number >= (1_u32 << 31) {
        return Err(problem("CSeq must be less than 2^31"));
    }
    Ok(number)
}

fn top_branch(headers: &Headers) -> Result<&str> {
    let via = required(headers, "via")?;
    // Deliberately reject syntax not implemented by this direct-client module.
    if via.contains([',', '"', '(', ')']) {
        return Err(problem("Combined/commented Via is not supported here"));
    }
    let mut branch = None;
    for parameter in via.split(';').skip(1) {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("branch") {
            let value = value.trim();
            if branch.is_some()
                || !value.starts_with("z9hG4bK")
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
            {
                return Err(problem("Invalid or duplicate Via branch"));
            }
            branch = Some(value);
        }
    }
    branch.ok_or_else(|| problem("Missing Via branch"))
}

fn problem(text: impl Into<String>) -> InviteControlError {
    InviteControlError(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sip::message::Message;

    fn invite() -> Request {
        let text = concat!(
            "INVITE sip:bob@127.0.0.1:5070 SIP/2.0\r\n",
            "Via: SIP/2.0/UDP 127.0.0.1:5072;branch=z9hG4bK-original;rport\r\n",
            "From: <sip:alice@local.test>;tag=alice\r\n",
            "To: <sip:bob@local.test>\r\n",
            "Call-ID: test-invite\r\n",
            "CSeq: 7 INVITE\r\n",
            "Max-Forwards: 69\r\n",
            "Route: <sip:first.local.test;lr>\r\n",
            "Route: <sip:second.local.test;lr>\r\n",
            "Content-Length: 0\r\n\r\n"
        );
        match Message::parse(text).expect("valid request") {
            Message::Request(request) => request,
            Message::Response(_) => panic!("expected request"),
        }
    }

    fn response(status: u16) -> Response {
        let text = format!(
            "SIP/2.0 {status} Test\r\n\
             Via: SIP/2.0/UDP 127.0.0.1:5072;branch=z9hG4bK-original;rport=5072;received=127.0.0.1\r\n\
             From: <sip:alice@local.test>;tag=alice\r\n\
             To: <sip:bob@local.test>;tag=bob\r\n\
             Call-ID: test-invite\r\n\
             CSeq: 7 INVITE\r\n\
             Content-Length: 0\r\n\r\n"
        );
        match Message::parse(&text).expect("valid response") {
            Message::Response(response) => response,
            Message::Request(_) => panic!("expected response"),
        }
    }

    #[test]
    fn cancel_preserves_invite_transaction_and_routes() {
        let original = invite();
        let cancel = build_cancel(&original).expect("valid CANCEL");
        assert_eq!(cancel.method, Method::Cancel);
        assert_eq!(cancel.target, original.target);
        assert_eq!(cancel.headers.get("cseq"), Some("7 CANCEL"));
        assert_eq!(cancel.headers.get("via"), original.headers.get("via"));
        assert_eq!(cancel.headers.get("to"), original.headers.get("to"));
        assert_eq!(
            cancel.headers.get_all("route"),
            original.headers.get_all("route")
        );
        assert_eq!(cancel.headers.get("max-forwards"), Some("69"));
        assert!(cancel.body.is_empty());
    }

    #[test]
    fn failure_ack_uses_response_to_and_original_branch() {
        let original = invite();
        let reply = response(486);
        let ack = build_failure_ack(&original, &reply).expect("valid ACK");
        assert_eq!(ack.method, Method::Ack);
        assert_eq!(ack.headers.get("cseq"), Some("7 ACK"));
        assert_eq!(ack.headers.get("to"), reply.headers.get("to"));
        assert_eq!(ack.headers.get("via"), original.headers.get("via"));
        assert_eq!(ack.target, original.target);
        assert!(ack.body.is_empty());
    }

    #[test]
    fn ack_rejects_successful_and_provisional_responses() {
        for status in [180, 200, 202] {
            assert!(build_failure_ack(&invite(), &response(status)).is_err());
        }
    }

    #[test]
    fn ack_rejects_other_transaction() {
        let original = invite();
        for name in ["call-id", "cseq", "via", "from"] {
            let mut reply = response(487);
            let value = match name {
                "cseq" => "8 INVITE",
                "via" => "SIP/2.0/UDP 127.0.0.1:5072;branch=z9hG4bK-other",
                "from" => "<sip:other@local.test>;tag=other",
                _ => "another-call",
            };
            reply.headers.insert(name, value).expect("valid header");
            assert!(
                build_failure_ack(&original, &reply).is_err(),
                "header: {name}"
            );
        }
    }

    #[test]
    fn ack_accepts_a_from_address_rewritten_by_the_server() {
        let original = invite();
        let mut reply = response(401);
        // Same caller tag, different spelling of the address.
        reply
            .headers
            .insert("from", "\"Alice\" <sip:alice@192.0.2.1>;tag=alice")
            .expect("valid header");
        let ack = build_failure_ack(&original, &reply).expect("valid ACK");
        // The ACK repeats the From of the INVITE, not of the response.
        assert_eq!(ack.headers.get("from"), original.headers.get("from"));

        reply
            .headers
            .insert("from", "<sip:alice@local.test>")
            .expect("valid header");
        assert!(build_failure_ack(&original, &reply).is_err());
    }

    #[test]
    fn rejects_non_invite_original() {
        let mut original = invite();
        original.method = Method::Bye;
        assert!(build_cancel(&original).is_err());
        assert!(build_failure_ack(&original, &response(486)).is_err());
    }

    #[test]
    fn generated_requests_round_trip() {
        let original = invite();
        let requests = [
            build_cancel(&original).expect("valid CANCEL"),
            build_failure_ack(&original, &response(487)).expect("valid ACK"),
        ];
        for request in requests {
            let bytes = request.serialize().expect("serialize");
            let text = std::str::from_utf8(&bytes).expect("UTF-8");
            assert_eq!(
                Message::parse(text).expect("parse"),
                Message::Request(request)
            );
        }
    }
}
