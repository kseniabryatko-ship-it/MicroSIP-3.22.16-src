pub mod client;
pub mod digest;
pub mod message;
pub mod sdp;
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SipScheme {
    Sip,
    Sips,
}

impl SipScheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sip => "sip",
            Self::Sips => "sips",
        }
    }

    fn parse(value: &str) -> Result<Self, SipUriError> {
        if value.eq_ignore_ascii_case("sip") {
            Ok(Self::Sip)
        } else if value.eq_ignore_ascii_case("sips") {
            Ok(Self::Sips)
        } else {
            Err(SipUriError::new("unsupported URI scheme"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
    Tls,
}

impl Transport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("udp") {
            Some(Self::Udp)
        } else if value.eq_ignore_ascii_case("tcp") {
            Some(Self::Tcp)
        } else if value.eq_ignore_ascii_case("tls") {
            Some(Self::Tls)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipUriError {
    message: String,
}

impl SipUriError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SipUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SipUriError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipUri {
    pub scheme: SipScheme,
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub transport: Option<Transport>,
    pub parameters: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

impl SipUri {
    pub fn parse(input: &str) -> Result<Self, SipUriError> {
        if input.is_empty() {
            return Err(SipUriError::new("SIP URI is empty"));
        }

        if input.chars().any(|character| character.is_whitespace()) {
            return Err(SipUriError::new("SIP URI contains whitespace"));
        }

        let (scheme_text, remainder) = input
            .split_once(':')
            .ok_or_else(|| SipUriError::new("SIP URI has no scheme"))?;

        let scheme = SipScheme::parse(scheme_text)?;

        let (before_headers, headers_part) = match remainder.find('?') {
            Some(position) => (&remainder[..position], Some(&remainder[position + 1..])),
            None => (remainder, None),
        };

        let (authority, parameters_part) = match before_headers.find(';') {
            Some(position) => (
                &before_headers[..position],
                Some(&before_headers[position + 1..]),
            ),
            None => (before_headers, None),
        };

        if authority.is_empty() {
            return Err(SipUriError::new("SIP URI has no host"));
        }

        let (user_part, host_port) = match authority.split_once('@') {
            Some((user, host)) => {
                if user.is_empty() {
                    return Err(SipUriError::new("SIP URI has an empty user"));
                }

                if user.contains('@') {
                    return Err(SipUriError::new("SIP URI contains an invalid '@'"));
                }

                (Some(user), host)
            }
            None => (None, authority),
        };

        let user = match user_part {
            Some(value) => Some(percent_decode(value)?),
            None => None,
        };

        let (host, port) = parse_host_port(host_port)?;

        let mut parameters = Vec::new();
        let mut transport = None;

        if let Some(part) = parameters_part {
            for raw_parameter in part.split(';') {
                if raw_parameter.is_empty() {
                    continue;
                }

                let (name, value) = match raw_parameter.split_once('=') {
                    Some((name, value)) => (name, value),
                    None => (raw_parameter, ""),
                };

                if name.is_empty() {
                    return Err(SipUriError::new("SIP URI has an empty parameter name"));
                }

                let normalized_name = name.to_ascii_lowercase();

                if normalized_name == "transport" {
                    if transport.is_some() {
                        return Err(SipUriError::new(
                            "SIP URI contains duplicate transport parameters",
                        ));
                    }

                    transport = Some(Transport::parse(value).ok_or_else(|| {
                        SipUriError::new("SIP URI contains an unsupported transport")
                    })?);
                } else {
                    parameters.push((normalized_name, percent_decode(value)?));
                }
            }
        }

        let mut headers = Vec::new();

        if let Some(part) = headers_part {
            for raw_header in part.split('&') {
                if raw_header.is_empty() {
                    continue;
                }

                let (name, value) = raw_header
                    .split_once('=')
                    .ok_or_else(|| SipUriError::new("SIP URI header has no value"))?;

                if name.is_empty() {
                    return Err(SipUriError::new("SIP URI has an empty header name"));
                }

                headers.push((name.to_ascii_lowercase(), percent_decode(value)?));
            }
        }

        Ok(Self {
            scheme,
            user,
            host,
            port,
            transport,
            parameters,
            headers,
        })
    }

    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(parameter_name, _)| parameter_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub const fn default_port(&self) -> u16 {
        match self.scheme {
            SipScheme::Sip => 5060,
            SipScheme::Sips => 5061,
        }
    }

    pub const fn effective_port(&self) -> u16 {
        match self.port {
            Some(port) => port,
            None => self.default_port(),
        }
    }
}

fn parse_host_port(input: &str) -> Result<(String, Option<u16>), SipUriError> {
    if input.starts_with('[') {
        let closing_bracket = input
            .find(']')
            .ok_or_else(|| SipUriError::new("IPv6 address has no closing bracket"))?;

        let host = &input[1..closing_bracket];
        let remainder = &input[closing_bracket + 1..];

        if host.is_empty() {
            return Err(SipUriError::new("IPv6 address is empty"));
        }

        let port = if remainder.is_empty() {
            None
        } else {
            let port_text = remainder
                .strip_prefix(':')
                .ok_or_else(|| SipUriError::new("invalid characters after IPv6 address"))?;

            Some(parse_port(port_text)?)
        };

        return Ok((host.to_owned(), port));
    }

    if input.contains(']') {
        return Err(SipUriError::new("invalid IPv6 address"));
    }

    if input.matches(':').count() > 1 {
        return Err(SipUriError::new(
            "IPv6 addresses must be enclosed in brackets",
        ));
    }

    let (host, port_text) = match input.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (input, None),
    };

    if host.is_empty() {
        return Err(SipUriError::new("SIP URI has an empty host"));
    }

    let port = match port_text {
        Some(value) => Some(parse_port(value)?),
        None => None,
    };

    Ok((host.to_owned(), port))
}

fn parse_port(value: &str) -> Result<u16, SipUriError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| SipUriError::new("invalid SIP URI port"))?;

    if port == 0 {
        return Err(SipUriError::new("SIP URI port cannot be zero"));
    }

    Ok(port)
}

fn percent_decode(input: &str) -> Result<String, SipUriError> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];

        if byte == b'%' {
            if index + 2 >= bytes.len() {
                return Err(SipUriError::new("incomplete percent escape"));
            }

            let high = hex_value(bytes[index + 1])
                .ok_or_else(|| SipUriError::new("invalid percent escape"))?;
            let low = hex_value(bytes[index + 2])
                .ok_or_else(|| SipUriError::new("invalid percent escape"))?;

            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }

    String::from_utf8(decoded)
        .map_err(|_| SipUriError::new("percent escape contains invalid UTF-8"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_uri() {
        let uri = SipUri::parse(
            "sip:alice@example.com:5060;transport=tcp;user=phone?subject=hello%20world",
        )
        .expect("URI should be valid");

        assert_eq!(uri.scheme, SipScheme::Sip);
        assert_eq!(uri.user.as_deref(), Some("alice"));
        assert_eq!(uri.host, "example.com");
        assert_eq!(uri.port, Some(5060));
        assert_eq!(uri.transport, Some(Transport::Tcp));
        assert_eq!(uri.parameter("user"), Some("phone"));
        assert_eq!(uri.header("subject"), Some("hello world"));
        assert_eq!(uri.effective_port(), 5060);
    }

    #[test]
    fn parses_secure_uri_with_ipv6_address() {
        let uri =
            SipUri::parse("sips:[2001:db8::1]:5062;transport=tls").expect("URI should be valid");

        assert_eq!(uri.scheme, SipScheme::Sips);
        assert_eq!(uri.host, "2001:db8::1");
        assert_eq!(uri.port, Some(5062));
        assert_eq!(uri.transport, Some(Transport::Tls));
        assert_eq!(uri.default_port(), 5061);
    }

    #[test]
    fn decodes_percent_escaped_user() {
        let uri =
            SipUri::parse("sip:pop%40voice.example.com@example.com").expect("URI should be valid");

        assert_eq!(uri.user.as_deref(), Some("pop@voice.example.com"));
    }

    #[test]
    fn parses_parameter_without_value() {
        let uri = SipUri::parse("sip:example.com;lr").expect("URI should be valid");

        assert_eq!(uri.parameter("lr"), Some(""));
    }

    #[test]
    fn rejects_malformed_uris() {
        let invalid_uris = [
            "example.com",
            "http://example.com",
            "sip:",
            "sip:alice@example.com:70000",
            "sip:alice@example.com:0",
            "sip:[2001:db8::1",
            "sip:alice@example.com;transport=unknown",
            "sip:alice%ZZ@example.com",
            "sip:alice@example.com?subject",
        ];

        for input in invalid_uris {
            assert!(
                SipUri::parse(input).is_err(),
                "URI should be invalid: {input}"
            );
        }
    }
}
