use crate::sip::message::{Headers, Message};
use crate::sip::transaction::TransactionKey;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderError(String);

impl fmt::Display for HeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for HeaderError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionHeaders {
    pub branch: String,
    pub sent_by: String,
    pub cseq: u32,
    pub method: String,
    pub call_id: String,
    pub from: String,
    pub to: String,
}

impl TransactionHeaders {
    pub fn from_message(message: &Message) -> Result<Self, HeaderError> {
        let headers = message.headers();
        let via = top_via(headers)?;
        let (sent_by, branch) = parse_via(via)?;
        let cseq_value = single_header(headers, "cseq", None)?;
        let (cseq, method) = parse_cseq(cseq_value)?;

        if let Message::Request(request) = message
            && request.method.as_str() != method
        {
            return Err(HeaderError(
                "Request method does not match CSeq method".to_owned(),
            ));
        }

        let call_id = single_header(headers, "call-id", Some("i"))?;
        if call_id.chars().any(char::is_whitespace) {
            return Err(HeaderError("Call-ID contains whitespace".to_owned()));
        }

        Ok(Self {
            branch,
            sent_by,
            cseq,
            method,
            call_id: call_id.to_owned(),
            from: single_header(headers, "from", Some("f"))?.to_owned(),
            to: single_header(headers, "to", Some("t"))?.to_owned(),
        })
    }

    pub fn key(&self) -> Result<TransactionKey, HeaderError> {
        TransactionKey::new(&self.branch, &self.method)
            .map_err(|error| HeaderError(error.to_string()))
    }

    /// Для обычного ответа метод должен совпадать с методом запроса.
    /// Особые правила сопоставления ACK/CANCEL сюда не входят.
    pub fn matches_response(&self, response: &Self) -> bool {
        self.branch == response.branch
            && self.sent_by.eq_ignore_ascii_case(&response.sent_by)
            && self.cseq == response.cseq
            && self.method == response.method
            && self.call_id == response.call_id
    }
}

pub fn response_matches_request(
    request: &Message,
    response: &Message,
) -> Result<bool, HeaderError> {
    if !matches!(request, Message::Request(_)) {
        return Err(HeaderError("Expected a SIP request".to_owned()));
    }
    if !matches!(response, Message::Response(_)) {
        return Err(HeaderError("Expected a SIP response".to_owned()));
    }

    let request_headers = TransactionHeaders::from_message(request)?;
    let response_headers = TransactionHeaders::from_message(response)?;
    Ok(request_headers.matches_response(&response_headers))
}

fn single_header<'a>(
    headers: &'a Headers,
    name: &str,
    compact: Option<&str>,
) -> Result<&'a str, HeaderError> {
    let mut matching = headers.iter().filter(|header| {
        header.name.eq_ignore_ascii_case(name)
            || compact.is_some_and(|alias| header.name.eq_ignore_ascii_case(alias))
    });

    let header = matching
        .next()
        .ok_or_else(|| HeaderError(format!("Missing {name} header")))?;

    if matching.next().is_some() {
        return Err(HeaderError(format!("Duplicate {name} header")));
    }

    let value = header.value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(HeaderError(format!("Invalid {name} header")));
    }

    Ok(value)
}

fn top_via(headers: &Headers) -> Result<&str, HeaderError> {
    let header = headers
        .iter()
        .find(|header| {
            header.name.eq_ignore_ascii_case("via") || header.name.eq_ignore_ascii_case("v")
        })
        .ok_or_else(|| HeaderError("Missing Via header".to_owned()))?;

    if header.value.chars().any(char::is_control) {
        return Err(HeaderError("Invalid Via header".to_owned()));
    }

    // Поддерживаем обычный Via без quoted-string/comment.
    // Не разбираем неоднозначные запятые внутри таких конструкций.
    if header.value.contains(['"', '(', ')']) {
        return Err(HeaderError(
            "Via quoted strings and comments are not supported yet".to_owned(),
        ));
    }

    header
        .value
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| HeaderError("Empty Via header".to_owned()))
}

fn parse_via(value: &str) -> Result<(String, String), HeaderError> {
    let mut parts = value.split(';');
    let mut address = parts.next().unwrap_or_default().split_whitespace();
    let protocol = address.next().unwrap_or_default();
    let sent_by = address.next().unwrap_or_default();

    if !matches!(protocol, "SIP/2.0/UDP" | "SIP/2.0/TCP" | "SIP/2.0/TLS")
        || sent_by.is_empty()
        || address.next().is_some()
    {
        return Err(HeaderError(
            "Invalid or unsupported Via protocol/address".to_owned(),
        ));
    }

    let mut branch = None;
    for parameter in parts {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("branch") {
            if branch.is_some() {
                return Err(HeaderError("Duplicate Via branch".to_owned()));
            }
            let value = value.trim();
            if !value.starts_with("z9hG4bK") || !is_token(value) {
                return Err(HeaderError("Invalid RFC 3261 Via branch".to_owned()));
            }
            branch = Some(value.to_owned());
        }
    }

    let branch = branch.ok_or_else(|| HeaderError("Missing Via branch".to_owned()))?;
    Ok((sent_by.to_owned(), branch))
}

fn parse_cseq(value: &str) -> Result<(u32, String), HeaderError> {
    let mut parts = value.split_whitespace();
    let number = parts.next().unwrap_or_default();
    let method = parts.next().unwrap_or_default();

    if number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || !is_token(method)
        || parts.next().is_some()
    {
        return Err(HeaderError("Invalid CSeq syntax".to_owned()));
    }

    let number = number
        .parse::<u32>()
        .map_err(|_| HeaderError("Invalid CSeq number".to_owned()))?;

    if number >= (1_u32 << 31) {
        return Err(HeaderError("CSeq must be less than 2^31".to_owned()));
    }

    Ok((number, method.to_owned()))
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(cseq: &str) -> Message {
        let text = format!(
            "INVITE sip:bob@local.test SIP/2.0\r\n\
             Via: SIP/2.0/UDP 127.0.0.1:5062;branch=z9hG4bK-test\r\n\
             From: <sip:alice@local.test>;tag=alice\r\n\
             To: <sip:bob@local.test>\r\n\
             Call-ID: test-call\r\n\
             CSeq: {cseq}\r\n\
             Content-Length: 0\r\n\r\n"
        );
        Message::parse(&text).expect("request should parse")
    }

    fn response(branch: &str) -> Message {
        let text = format!(
            "SIP/2.0 200 OK\r\n\
             v: SIP/2.0/UDP 127.0.0.1:5062;branch={branch}\r\n\
             f: <sip:alice@local.test>;tag=alice\r\n\
             t: <sip:bob@local.test>;tag=bob\r\n\
             i: test-call\r\n\
             CSeq: 1 INVITE\r\n\
             Content-Length: 0\r\n\r\n"
        );
        Message::parse(&text).expect("response should parse")
    }

    #[test]
    fn extracts_key_from_request() {
        let metadata = TransactionHeaders::from_message(&request("1 INVITE"))
            .expect("metadata should be valid");
        let key = metadata.key().expect("key should be valid");
        assert_eq!(key.branch(), "z9hG4bK-test");
        assert_eq!(key.method(), "INVITE");
        assert_eq!(metadata.cseq, 1);
    }

    #[test]
    fn matches_response_with_compact_headers() {
        assert!(
            response_matches_request(&request("1 INVITE"), &response("z9hG4bK-test"))
                .expect("headers should be valid")
        );
    }

    #[test]
    fn rejects_different_branch() {
        assert!(
            !response_matches_request(&request("1 INVITE"), &response("z9hG4bK-other"))
                .expect("headers should be valid")
        );
    }

    #[test]
    fn rejects_invalid_cseq() {
        for cseq in ["1 BYE", "2147483648 INVITE", "+1 INVITE", "1 INVITE extra"] {
            assert!(TransactionHeaders::from_message(&request(cseq)).is_err());
        }
    }
}
