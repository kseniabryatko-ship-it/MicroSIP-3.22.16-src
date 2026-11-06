use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdpError {
    message: String,
}

impl SdpError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SdpError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub username: String,
    pub session_id: String,
    pub session_version: String,
    pub network_type: String,
    pub address_type: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub network_type: String,
    pub address_type: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeDescription {
    pub start: u64,
    pub stop: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
    Application,
    Other(String),
}

impl MediaKind {
    fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("audio") {
            Self::Audio
        } else if value.eq_ignore_ascii_case("video") {
            Self::Video
        } else if value.eq_ignore_ascii_case("application") {
            Self::Application
        } else {
            Self::Other(value.to_owned())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    pub kind: MediaKind,
    pub port: u16,
    pub port_count: Option<u16>,
    pub protocol: String,
    pub formats: Vec<String>,
    pub connection: Option<Connection>,
    pub attributes: Vec<Attribute>,
}

impl MediaDescription {
    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdp {
    pub version: u8,
    pub origin: Origin,
    pub session_name: String,
    pub connection: Option<Connection>,
    pub times: Vec<TimeDescription>,
    pub attributes: Vec<Attribute>,
    pub media: Vec<MediaDescription>,
}

impl Sdp {
    pub fn parse(input: &str) -> Result<Self, SdpError> {
        if input.trim().is_empty() {
            return Err(SdpError::new("SDP is empty"));
        }

        let mut version = None;
        let mut origin = None;
        let mut session_name = None;
        let mut session_connection = None;
        let mut times = Vec::new();
        let mut session_attributes = Vec::new();
        let mut media = Vec::new();
        let mut current_media: Option<MediaDescription> = None;

        for raw_line in input.lines() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

            if line.trim().is_empty() {
                continue;
            }

            let (field, value) = line
                .split_once('=')
                .ok_or_else(|| SdpError::new("SDP line has no '='"))?;

            if field.len() != 1
                || !field
                    .chars()
                    .all(|character| character.is_ascii_alphabetic())
            {
                return Err(SdpError::new(format!("Invalid SDP field: {field}")));
            }

            match field {
                "v" => {
                    if version.is_some() {
                        return Err(SdpError::new("SDP contains multiple version lines"));
                    }

                    let parsed = value
                        .parse::<u8>()
                        .map_err(|_| SdpError::new("Invalid SDP version"))?;

                    if parsed != 0 {
                        return Err(SdpError::new("Only SDP version 0 is supported"));
                    }

                    version = Some(parsed);
                }

                "o" => {
                    if origin.is_some() {
                        return Err(SdpError::new("SDP contains multiple origin lines"));
                    }

                    origin = Some(parse_origin(value)?);
                }

                "s" => {
                    if session_name.is_some() {
                        return Err(SdpError::new("SDP contains multiple session names"));
                    }

                    if value.is_empty() {
                        return Err(SdpError::new("SDP session name is empty"));
                    }

                    session_name = Some(value.to_owned());
                }

                "c" => {
                    let connection = parse_connection(value)?;

                    if let Some(current) = current_media.as_mut() {
                        if current.connection.is_some() {
                            return Err(SdpError::new(
                                "SDP media section contains multiple connection lines",
                            ));
                        }

                        current.connection = Some(connection);
                    } else {
                        if session_connection.is_some() {
                            return Err(SdpError::new(
                                "SDP contains multiple session connection lines",
                            ));
                        }

                        session_connection = Some(connection);
                    }
                }

                "t" => {
                    times.push(parse_time(value)?);
                }

                "a" => {
                    let attribute = parse_attribute(value);

                    if let Some(current) = current_media.as_mut() {
                        current.attributes.push(attribute);
                    } else {
                        session_attributes.push(attribute);
                    }
                }

                "m" => {
                    if let Some(previous) = current_media.take() {
                        media.push(previous);
                    }

                    current_media = Some(parse_media(value)?);
                }

                // Остальные стандартные поля SDP пока не нужны.
                _ => {}
            }
        }

        if let Some(last_media) = current_media {
            media.push(last_media);
        }

        let version = version.ok_or_else(|| SdpError::new("SDP has no version line"))?;

        let origin = origin.ok_or_else(|| SdpError::new("SDP has no origin line"))?;

        let session_name = session_name.ok_or_else(|| SdpError::new("SDP has no session name"))?;

        if times.is_empty() {
            return Err(SdpError::new("SDP has no time description"));
        }

        if media.is_empty() {
            return Err(SdpError::new("SDP has no media descriptions"));
        }

        if let Some(connection) = &session_connection {
            for description in &mut media {
                if description.connection.is_none() {
                    description.connection = Some(connection.clone());
                }
            }
        }

        Ok(Self {
            version,
            origin,
            session_name,
            connection: session_connection,
            times,
            attributes: session_attributes,
            media,
        })
    }

    pub fn media(&self, kind: &MediaKind) -> Option<&MediaDescription> {
        self.media.iter().find(|media| &media.kind == kind)
    }

    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
    }
}

fn parse_origin(value: &str) -> Result<Origin, SdpError> {
    let fields: Vec<&str> = value.split_whitespace().collect();

    if fields.len() != 6 {
        return Err(SdpError::new("SDP origin must contain six fields"));
    }

    Ok(Origin {
        username: fields[0].to_owned(),
        session_id: fields[1].to_owned(),
        session_version: fields[2].to_owned(),
        network_type: fields[3].to_owned(),
        address_type: fields[4].to_owned(),
        address: fields[5].to_owned(),
    })
}

fn parse_connection(value: &str) -> Result<Connection, SdpError> {
    let fields: Vec<&str> = value.split_whitespace().collect();

    if fields.len() != 3 {
        return Err(SdpError::new("SDP connection must contain three fields"));
    }

    Ok(Connection {
        network_type: fields[0].to_owned(),
        address_type: fields[1].to_owned(),
        address: fields[2].to_owned(),
    })
}

fn parse_time(value: &str) -> Result<TimeDescription, SdpError> {
    let fields: Vec<&str> = value.split_whitespace().collect();

    if fields.len() != 2 {
        return Err(SdpError::new("SDP time must contain start and stop values"));
    }

    let start = fields[0]
        .parse::<u64>()
        .map_err(|_| SdpError::new("Invalid SDP start time"))?;

    let stop = fields[1]
        .parse::<u64>()
        .map_err(|_| SdpError::new("Invalid SDP stop time"))?;

    Ok(TimeDescription { start, stop })
}

fn parse_media(value: &str) -> Result<MediaDescription, SdpError> {
    let fields: Vec<&str> = value.split_whitespace().collect();

    if fields.len() < 4 {
        return Err(SdpError::new(
            "SDP media line must contain media, port, protocol and format",
        ));
    }

    let (port_text, port_count) = match fields[1].split_once('/') {
        Some((port, count)) => {
            let parsed = count
                .parse::<u16>()
                .map_err(|_| SdpError::new("Invalid SDP media port count"))?;

            if parsed == 0 {
                return Err(SdpError::new("SDP media port count cannot be zero"));
            }

            (port, Some(parsed))
        }
        None => (fields[1], None),
    };

    let port = port_text
        .parse::<u16>()
        .map_err(|_| SdpError::new("Invalid SDP media port"))?;

    Ok(MediaDescription {
        kind: MediaKind::parse(fields[0]),
        port,
        port_count,
        protocol: fields[2].to_owned(),
        formats: fields[3..]
            .iter()
            .map(|format| format.to_string())
            .collect(),
        connection: None,
        attributes: Vec::new(),
    })
}

fn parse_attribute(value: &str) -> Attribute {
    match value.split_once(':') {
        Some((name, attribute_value)) => Attribute {
            name: name.to_ascii_lowercase(),
            value: Some(attribute_value.to_owned()),
        },
        None => Attribute {
            name: value.to_ascii_lowercase(),
            value: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_audio_and_video_sdp() {
        let input = concat!(
            "v=0\r\n",
            "o=alice 289084452 289084452 IN IP4 192.0.2.10\r\n",
            "s=MicroSIP Rust\r\n",
            "c=IN IP4 192.0.2.10\r\n",
            "t=0 0\r\n",
            "m=audio 40000 RTP/AVP 0 8\r\n",
            "a=rtpmap:0 PCMU/8000\r\n",
            "a=rtpmap:8 PCMA/8000\r\n",
            "m=video 40002 RTP/AVP 96\r\n",
            "a=rtpmap:96 H264/90000\r\n",
        );

        let sdp = Sdp::parse(input).expect("SDP should be valid");

        assert_eq!(sdp.version, 0);
        assert_eq!(sdp.origin.username, "alice");
        assert_eq!(sdp.session_name, "MicroSIP Rust");
        assert_eq!(sdp.media.len(), 2);

        let audio = sdp.media(&MediaKind::Audio).expect("audio should exist");

        assert_eq!(audio.port, 40000);
        assert_eq!(audio.formats, vec![String::from("0"), String::from("8")]);

        assert_eq!(
            audio
                .attribute("rtpmap")
                .and_then(|attribute| attribute.value.as_deref()),
            Some("0 PCMU/8000")
        );

        let video = sdp.media(&MediaKind::Video).expect("video should exist");

        assert_eq!(video.port, 40002);
    }

    #[test]
    fn inherits_session_connection() {
        let input = concat!(
            "v=0\r\n",
            "o=alice 1 1 IN IP4 192.0.2.10\r\n",
            "s=Test\r\n",
            "c=IN IP4 192.0.2.10\r\n",
            "t=0 0\r\n",
            "m=audio 40000 RTP/AVP 0\r\n",
        );

        let sdp = Sdp::parse(input).expect("SDP should be valid");

        let audio = sdp.media(&MediaKind::Audio).expect("audio should exist");

        let connection = audio
            .connection
            .as_ref()
            .expect("connection should be inherited");

        assert_eq!(connection.address, "192.0.2.10");
    }

    #[test]
    fn parses_media_port_count() {
        let input = concat!(
            "v=0\r\n",
            "o=alice 1 1 IN IP4 192.0.2.10\r\n",
            "s=Test\r\n",
            "t=0 0\r\n",
            "m=audio 40000/2 RTP/AVP 0\r\n",
        );

        let sdp = Sdp::parse(input).expect("SDP should be valid");

        let audio = sdp.media(&MediaKind::Audio).expect("audio should exist");

        assert_eq!(audio.port, 40000);
        assert_eq!(audio.port_count, Some(2));
    }

    #[test]
    fn rejects_malformed_sdp() {
        let invalid_inputs = [
            "",
            "v=1\r\n",
            concat!(
                "v=0\r\n",
                "s=Test\r\n",
                "t=0 0\r\n",
                "m=audio 40000 RTP/AVP 0\r\n",
            ),
            concat!(
                "v=0\r\n",
                "o=alice 1 1 IN IP4 192.0.2.10\r\n",
                "s=Test\r\n",
                "t=0 0\r\n",
            ),
            concat!(
                "v=0\r\n",
                "o=alice 1 1 IN IP4 192.0.2.10\r\n",
                "s=Test\r\n",
                "t=0 0\r\n",
                "m=audio invalid RTP/AVP 0\r\n",
            ),
        ];

        for input in invalid_inputs {
            assert!(Sdp::parse(input).is_err(), "SDP should be invalid: {input}");
        }
    }
}
