use md5::{Digest, Md5};
use std::error::Error;
use std::fmt::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestError {
    message: String,
}

impl DigestError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DigestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub algorithm: Option<String>,
    pub qop: Option<String>,
    pub stale: Option<String>,
}

impl DigestChallenge {
    pub fn parse(input: &str) -> Result<Self, DigestError> {
        let trimmed = input.trim();

        if trimmed.is_empty() {
            return Err(DigestError::new("Digest challenge is empty"));
        }

        let parameter_text = match trimmed.as_bytes().get(..6) {
            Some(prefix) if prefix.eq_ignore_ascii_case(b"Digest") => &trimmed[6..],
            _ => trimmed,
        };

        let parameters = parse_parameters(parameter_text)?;

        let realm = parameter(&parameters, "realm")
            .ok_or_else(|| DigestError::new("Digest challenge has no realm parameter"))?
            .to_owned();

        let nonce = parameter(&parameters, "nonce")
            .ok_or_else(|| DigestError::new("Digest challenge has no nonce parameter"))?
            .to_owned();

        let opaque = parameter(&parameters, "opaque").map(str::to_owned);

        let algorithm = parameter(&parameters, "algorithm").map(|value| value.to_ascii_uppercase());

        if let Some(value) = &algorithm
            && value != "MD5"
            && value != "MD5-SESS"
        {
            return Err(DigestError::new(format!(
                "Unsupported Digest algorithm: {value}"
            )));
        }

        let qop = parameter(&parameters, "qop").and_then(|value| {
            if value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate.eq_ignore_ascii_case("auth"))
            {
                Some("auth".to_owned())
            } else {
                let value = value.trim();

                if value.is_empty() {
                    None
                } else {
                    Some(value.to_owned())
                }
            }
        });

        let stale = parameter(&parameters, "stale").map(str::to_owned);

        Ok(Self {
            realm,
            nonce,
            opaque,
            algorithm,
            qop,
            stale,
        })
    }
}

pub fn authorization_header(
    challenge: &DigestChallenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Result<String, DigestError> {
    let cnonce = generate_cnonce();

    authorization_header_with_cnonce(challenge, username, password, method, uri, &cnonce)
}

pub fn authorization_header_with_cnonce(
    challenge: &DigestChallenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    cnonce: &str,
) -> Result<String, DigestError> {
    if username.is_empty() {
        return Err(DigestError::new("Digest username cannot be empty"));
    }

    if uri.is_empty() {
        return Err(DigestError::new("Digest URI cannot be empty"));
    }

    if cnonce.is_empty() {
        return Err(DigestError::new("Digest client nonce cannot be empty"));
    }

    let algorithm = challenge
        .algorithm
        .as_deref()
        .unwrap_or("MD5")
        .to_ascii_uppercase();

    let mut ha1 = md5_hex(&format!("{username}:{}:{password}", challenge.realm));

    if algorithm == "MD5-SESS" {
        ha1 = md5_hex(&format!("{ha1}:{}:{cnonce}", challenge.nonce));
    }

    let ha2 = md5_hex(&format!("{method}:{uri}"));

    let qop_auth = challenge
        .qop
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("auth"));

    let response = if qop_auth {
        md5_hex(&format!(
            "{ha1}:{}:00000001:{cnonce}:auth:{ha2}",
            challenge.nonce
        ))
    } else if challenge
        .qop
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("auth-int"))
    {
        return Err(DigestError::new("Digest qop=auth-int is not supported yet"));
    } else if let Some(qop) = challenge.qop.as_deref() {
        return Err(DigestError::new(format!("Unsupported Digest qop: {qop}")));
    } else {
        md5_hex(&format!("{ha1}:{}:{ha2}", challenge.nonce))
    };

    let mut parts = vec![
        format!("username=\"{}\"", escape_quoted(username)),
        format!("realm=\"{}\"", escape_quoted(&challenge.realm)),
        format!("nonce=\"{}\"", escape_quoted(&challenge.nonce)),
        format!("uri=\"{}\"", escape_quoted(uri)),
        format!("response=\"{response}\""),
        format!("algorithm={algorithm}"),
    ];

    if qop_auth {
        parts.push("qop=auth".to_owned());
        parts.push("nc=00000001".to_owned());
        parts.push(format!("cnonce=\"{}\"", escape_quoted(cnonce)));
    } else if algorithm == "MD5-SESS" {
        parts.push(format!("cnonce=\"{}\"", escape_quoted(cnonce)));
    }

    if let Some(opaque) = &challenge.opaque {
        parts.push(format!("opaque=\"{}\"", escape_quoted(opaque)));
    }

    Ok(format!("Digest {}", parts.join(", ")))
}

fn parameter<'a>(parameters: &'a [(String, String)], name: &str) -> Option<&'a str> {
    parameters
        .iter()
        .find(|(parameter_name, _)| parameter_name == name)
        .map(|(_, value)| value.as_str())
}

fn parse_parameters(input: &str) -> Result<Vec<(String, String)>, DigestError> {
    let mut characters = input.chars().peekable();
    let mut parameters = Vec::new();

    loop {
        while let Some(&character) = characters.peek() {
            if character == ',' || character.is_whitespace() {
                characters.next();
            } else {
                break;
            }
        }

        if characters.peek().is_none() {
            break;
        }

        let mut name = String::new();

        while let Some(&character) = characters.peek() {
            if character == '=' || character == ',' || character.is_whitespace() {
                break;
            }

            name.push(character);
            characters.next();
        }

        if name.is_empty() {
            return Err(DigestError::new(
                "Digest challenge contains an invalid parameter name",
            ));
        }

        while let Some(&character) = characters.peek() {
            if character.is_whitespace() {
                characters.next();
            } else {
                break;
            }
        }

        match characters.next() {
            Some('=') => {}
            _ => {
                return Err(DigestError::new(format!(
                    "Digest parameter {name} has no value"
                )));
            }
        }

        while let Some(&character) = characters.peek() {
            if character.is_whitespace() {
                characters.next();
            } else {
                break;
            }
        }

        let value = if characters.peek() == Some(&'"') {
            characters.next();

            let mut value = String::new();
            let mut closed = false;

            while let Some(character) = characters.next() {
                match character {
                    '\\' => {
                        let escaped = characters.next().ok_or_else(|| {
                            DigestError::new("Digest challenge has an unfinished escape sequence")
                        })?;

                        value.push(escaped);
                    }
                    '"' => {
                        closed = true;
                        break;
                    }
                    _ => value.push(character),
                }
            }

            if !closed {
                return Err(DigestError::new(
                    "Digest challenge has an unterminated quoted value",
                ));
            }

            if let Some(&next) = characters.peek()
                && next != ','
                && !next.is_whitespace()
            {
                return Err(DigestError::new(
                    "Digest challenge has invalid characters after a quoted value",
                ));
            }

            value
        } else {
            let mut value = String::new();

            while let Some(&character) = characters.peek() {
                if character == ',' {
                    break;
                }

                value.push(character);
                characters.next();
            }

            value.trim().to_owned()
        };

        parameters.push((name.to_ascii_lowercase(), value));
    }

    Ok(parameters)
}

fn md5_hex(value: &str) -> String {
    let digest = Md5::digest(value.as_bytes());
    let mut output = String::with_capacity(32);

    for &byte in digest.iter() {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }

    output
}

fn escape_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn generate_cnonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("{timestamp:032x}{counter:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_digest_challenge() {
        let challenge = DigestChallenge::parse(
            "Digest realm=\"testrealm@host.com\", \
             nonce=\"dcd98b7102dd1f9101a4a4c8d99e9f9b\", \
             qop=\"auth\", algorithm=MD5",
        )
        .expect("challenge should be valid");

        assert_eq!(challenge.realm, "testrealm@host.com");
        assert_eq!(challenge.nonce, "dcd98b7102dd1f9101a4a4c8d99e9f9b");
        assert_eq!(challenge.qop.as_deref(), Some("auth"));
        assert_eq!(challenge.algorithm.as_deref(), Some("MD5"));
    }

    #[test]
    fn calculates_known_digest_response() {
        let challenge = DigestChallenge::parse(
            "Digest realm=\"testrealm@host.com\", \
             nonce=\"dcd98b7102dd1f9101a4a4c8d99e9f9b\", \
             qop=\"auth\"",
        )
        .expect("challenge should be valid");

        assert_eq!(
            md5_hex("Mufasa:testrealm@host.com:Circle Of Life"),
            "939e7578ed9e3c518a452acee763bce9"
        );

        assert_eq!(
            md5_hex("GET:/dir/index.html"),
            "39aff3a2bab6126f332b942af96d3366"
        );

        let header = authorization_header_with_cnonce(
            &challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "0a4f113b",
        )
        .expect("authorization should be calculated");

        assert!(
            header.contains("response=\"52b34ea170faac332766f025a4d37340\""),
            "generated header: {header}"
        );
    }
}
