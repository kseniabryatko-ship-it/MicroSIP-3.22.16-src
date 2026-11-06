use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipMessageError {
    message: String,
}

impl SipMessageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SipMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SipMessageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SipVersion {
    pub major: u8,
    pub minor: u8,
}

impl Default for SipVersion {
    fn default() -> Self {
        Self { major: 2, minor: 0 }
    }
}

impl SipVersion {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        let (protocol, version) = input
            .split_once('/')
            .ok_or_else(|| SipMessageError::new("SIP version has no protocol"))?;

        if !protocol.eq_ignore_ascii_case("SIP") {
            return Err(SipMessageError::new("unsupported SIP protocol"));
        }

        let mut components = version.split('.');

        let major = components
            .next()
            .ok_or_else(|| SipMessageError::new("SIP version has no major version"))?
            .parse::<u8>()
            .map_err(|_| SipMessageError::new("invalid SIP major version"))?;

        let minor = components
            .next()
            .ok_or_else(|| SipMessageError::new("SIP version has no minor version"))?
            .parse::<u8>()
            .map_err(|_| SipMessageError::new("invalid SIP minor version"))?;

        if components.next().is_some() {
            return Err(SipMessageError::new("invalid SIP version"));
        }

        Ok(Self { major, minor })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Register,
    Invite,
    Ack,
    Bye,
    Cancel,
    Options,
    Message,
    Info,
    Update,
    Prack,
    Subscribe,
    Notify,
    Publish,
    Refer,
    Other(String),
}

impl Method {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        if input.is_empty() {
            return Err(SipMessageError::new("SIP method is empty"));
        }

        let normalized = input.to_ascii_uppercase();

        Ok(match normalized.as_str() {
            "REGISTER" => Self::Register,
            "INVITE" => Self::Invite,
            "ACK" => Self::Ack,
            "BYE" => Self::Bye,
            "CANCEL" => Self::Cancel,
            "OPTIONS" => Self::Options,
            "MESSAGE" => Self::Message,
            "INFO" => Self::Info,
            "UPDATE" => Self::Update,
            "PRACK" => Self::Prack,
            "SUBSCRIBE" => Self::Subscribe,
            "NOTIFY" => Self::Notify,
            "PUBLISH" => Self::Publish,
            "REFER" => Self::Refer,
            _ => Self::Other(normalized),
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Register => "REGISTER",
            Self::Invite => "INVITE",
            Self::Ack => "ACK",
            Self::Bye => "BYE",
            Self::Cancel => "CANCEL",
            Self::Options => "OPTIONS",
            Self::Message => "MESSAGE",
            Self::Info => "INFO",
            Self::Update => "UPDATE",
            Self::Prack => "PRACK",
            Self::Subscribe => "SUBSCRIBE",
            Self::Notify => "NOTIFY",
            Self::Publish => "PUBLISH",
            Self::Refer => "REFER",
            Self::Other(value) => value.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<Header>,
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.value.as_str())
    }

    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.value.as_str())
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub target: String,
    pub version: SipVersion,
    pub headers: Headers,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub version: SipVersion,
    pub status_code: u16,
    pub reason: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Request(Request),
    Response(Response),
}

impl Message {
    pub fn parse(input: &str) -> Result<Self, SipMessageError> {
        let (header_section, body) = split_message(input)?;

        let mut lines = header_section.split('\n');

        let start_line = lines
            .next()
            .unwrap_or_default()
            .strip_suffix('\r')
            .unwrap_or_else(|| header_section.split('\n').next().unwrap_or_default());

        let headers = parse_headers(lines)?;

        match headers.content_length()? {
            Some(expected) if body.len() != expected => {
                return Err(SipMessageError::new(format!(
                    "SIP body length is {}, but Content-Length is {}",
                    body.len(),
                    expected
                )));
            }
            Some(_) => {}
            None if !body.is_empty() => {
                return Err(SipMessageError::new(
                    "SIP message has a body but no Content-Length header",
                ));
            }
            None => {}
        }

        let message = match StartLine::parse(start_line)? {
            StartLine::Request {
                method,
                target,
                version,
            } => Message::Request(Request {
                method,
                target,
                version,
                headers,
                body: body.as_bytes().to_vec(),
            }),
            StartLine::Response {
                version,
                status_code,
                reason,
            } => Message::Response(Response {
                version,
                status_code,
                reason,
                headers,
                body: body.as_bytes().to_vec(),
            }),
        };

        Ok(message)
    }

    pub fn headers(&self) -> &Headers {
        match self {
            Self::Request(request) => &request.headers,
            Self::Response(response) => &response.headers,
        }
    }

    pub fn body(&self) -> &[u8] {
        match self {
            Self::Request(request) => request.body.as_slice(),
            Self::Response(response) => response.body.as_slice(),
        }
    }

    pub fn version(&self) -> SipVersion {
        match self {
            Self::Request(request) => request.version,
            Self::Response(response) => response.version,
        }
    }
}

enum StartLine {
    Request {
        method: Method,
        target: String,
        version: SipVersion,
    },
    Response {
        version: SipVersion,
        status_code: u16,
        reason: String,
    },
}

impl StartLine {
    fn parse(input: &str) -> Result<Self, SipMessageError> {
        let parts: Vec<&str> = input.split_whitespace().collect();

        if parts.is_empty() {
            return Err(SipMessageError::new("SIP start line is empty"));
        }

        let is_response = parts[0]
            .split_once('/')
            .map(|(protocol, _)| protocol.eq_ignore_ascii_case("SIP"))
            .unwrap_or(false);

        if is_response {
            if parts.len() < 2 {
                return Err(SipMessageError::new("SIP response has no status code"));
            }

            let version = SipVersion::parse(parts[0])?;

            let status_code = parts[1]
                .parse::<u16>()
                .map_err(|_| SipMessageError::new("invalid SIP status code"))?;

            if !(100..=699).contains(&status_code) {
                return Err(SipMessageError::new("SIP status code is out of range"));
            }

            let reason = parts[2..].join(" ");

            Ok(Self::Response {
                version,
                status_code,
                reason,
            })
        } else {
            if parts.len() != 3 {
                return Err(SipMessageError::new(
                    "SIP request line must contain method, target and version",
                ));
            }

            let method = Method::parse(parts[0])?;
            let target = parts[1].to_owned();
            let version = SipVersion::parse(parts[2])?;

            Ok(Self::Request {
                method,
                target,
                version,
            })
        }
    }
}

fn split_message(input: &str) -> Result<(&str, &str), SipMessageError> {
    if let Some(position) = input.find("\r\n\r\n") {
        return Ok((&input[..position], &input[position + 4..]));
    }

    if let Some(position) = input.find("\n\n") {
        return Ok((&input[..position], &input[position + 2..]));
    }

    Err(SipMessageError::new(
        "SIP message has no end-of-headers separator",
    ))
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Headers, SipMessageError> {
    let mut headers = Headers::default();
    let mut last_header_index: Option<usize> = None;

    for raw_line in lines {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line.is_empty() {
            continue;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            let index = last_header_index.ok_or_else(|| {
                SipMessageError::new("SIP header continuation has no previous header")
            })?;

            let continuation = line.trim();

            if continuation.is_empty() {
                continue;
            }

            headers.entries[index].value.push(' ');
            headers.entries[index].value.push_str(continuation);
            continue;
        }

        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| SipMessageError::new("SIP header has no colon"))?;

        let name = name.trim();

        if !is_valid_header_name(name) {
            return Err(SipMessageError::new("SIP header has an invalid name"));
        }

        headers.entries.push(Header {
            name: name.to_ascii_lowercase(),
            value: value.trim().to_owned(),
        });

        last_header_index = Some(headers.entries.len() - 1);
    }

    Ok(headers)
}

fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(character)
        })
}

impl Headers {
    fn content_length(&self) -> Result<Option<usize>, SipMessageError> {
        let mut values = self.get_all("content-length").into_iter();

        let Some(first) = values.next() else {
            return Ok(None);
        };

        let expected = first
            .trim()
            .parse::<usize>()
            .map_err(|_| SipMessageError::new("invalid Content-Length header"))?;

        for value in values {
            let actual = value
                .trim()
                .parse::<usize>()
                .map_err(|_| SipMessageError::new("invalid Content-Length header"))?;

            if actual != expected {
                return Err(SipMessageError::new(
                    "SIP message contains conflicting Content-Length headers",
                ));
            }
        }

        Ok(Some(expected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_with_headers() {
        let input = concat!(
            "INVITE sip:bob@example.com SIP/2.0\r\n",
            "Via: SIP/2.0/TCP client.example.com:5060;branch=z9hG4bK1\r\n",
            "From: <sip:alice@example.com>;tag=1928301774\r\n",
            "To: <sip:bob@example.com>\r\n",
            "Call-ID: call-123@example.com\r\n",
            "CSeq: 1 INVITE\r\n",
            "Max-Forwards: 70\r\n",
            "Content-Length: 0\r\n",
            "\r\n"
        );

        let message = Message::parse(input).expect("SIP request should be valid");

        let Message::Request(request) = message else {
            panic!("message should be a request");
        };

        assert_eq!(request.method, Method::Invite);
        assert_eq!(request.method.as_str(), "INVITE");
        assert_eq!(request.target, "sip:bob@example.com");
        assert_eq!(request.version, SipVersion { major: 2, minor: 0 });
        assert_eq!(request.headers.get("call-id"), Some("call-123@example.com"));
        assert_eq!(request.headers.get("cseq"), Some("1 INVITE"));
        assert_eq!(request.headers.get_all("via").len(), 1);
        assert!(request.body.is_empty());
    }

    #[test]
    fn parses_response_with_body() {
        let input = concat!(
            "SIP/2.0 200 OK\r\n",
            "Via: SIP/2.0/UDP one.example.com;branch=one\r\n",
            "Via: SIP/2.0/UDP two.example.com;branch=two\r\n",
            "From: <sip:alice@example.com>;tag=abc\r\n",
            "To: <sip:bob@example.com>;tag=def\r\n",
            "Call-ID: call-123@example.com\r\n",
            "Content-Length: 5\r\n",
            "\r\n",
            "hello"
        );

        let message = Message::parse(input).expect("SIP response should be valid");

        let Message::Response(response) = message else {
            panic!("message should be a response");
        };

        assert_eq!(response.status_code, 200);
        assert_eq!(response.reason, "OK");
        assert_eq!(response.headers.get_all("via").len(), 2);
        assert_eq!(response.body.as_slice(), b"hello");
    }

    #[test]
    fn parses_folded_header() {
        let input = concat!(
            "SIP/2.0 100 Trying\r\n",
            "Subject: first line\r\n",
            " second line\r\n",
            "Content-Length: 0\r\n",
            "\r\n"
        );

        let message = Message::parse(input).expect("SIP response should be valid");

        assert_eq!(
            message.headers().get("subject"),
            Some("first line second line")
        );
    }

    #[test]
    fn rejects_malformed_messages() {
        let invalid_messages = [
            "",
            "INVITE sip:bob@example.com\r\n\r\n",
            "INVITE sip:bob@example.com HTTP/2.0\r\n\r\n",
            "INVITE sip:bob@example.com SIP/2.0\r\nBrokenHeader\r\n\r\n",
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Length: 1\r\n",
                "\r\n",
                "xy"
            ),
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Length: 1\r\n",
                "Content-Length: 2\r\n",
                "\r\n",
                "x"
            ),
            concat!(
                "INVITE sip:bob@example.com SIP/2.0\r\n",
                "Content-Type: text/plain\r\n",
                "\r\n",
                "body"
            ),
            concat!("SIP/2.0 999 Invalid\r\n", "Content-Length: 0\r\n", "\r\n"),
        ];

        for input in invalid_messages {
            assert!(
                Message::parse(input).is_err(),
                "message should be invalid: {input}"
            );
        }
    }
}
