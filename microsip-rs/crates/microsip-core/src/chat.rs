//! Text messages sent and received, kept in a plain text file next to the
//! settings, one message per line: `time  in|out  number  name  text`. Line
//! breaks and tabs inside a message are written as `\n` and `\t`.

use crate::history::field;
use std::fs;
use std::io;
use std::path::Path;

/// Messages kept; the oldest are dropped beyond this.
pub const CHAT_LIMIT: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Seconds since 1970-01-01 UTC.
    pub time: u64,
    pub incoming: bool,
    /// The other side.
    pub number: String,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chat {
    /// Oldest first, as a conversation is read.
    messages: Vec<ChatMessage>,
}

impl Chat {
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn add(&mut self, message: ChatMessage) {
        self.messages.push(message);
        let excess = self.messages.len().saturating_sub(CHAT_LIMIT);
        self.messages.drain(..excess);
    }

    /// The messages exchanged with one number, oldest first.
    pub fn with(&self, number: &str) -> Vec<&ChatMessage> {
        self.messages
            .iter()
            .filter(|message| message.number == number)
            .collect()
    }

    /// Everyone written with, the most recent conversation first, with the
    /// name last seen for them.
    pub fn partners(&self) -> Vec<(String, String)> {
        let mut partners: Vec<(String, String)> = Vec::new();
        for message in self.messages.iter().rev() {
            if !partners.iter().any(|(number, _)| *number == message.number) {
                partners.push((message.number.clone(), message.name.clone()));
            }
        }
        partners
    }

    /// Received messages newer than `since`.
    pub fn received_since(&self, since: u64) -> usize {
        self.messages
            .iter()
            .filter(|message| message.incoming && message.time > since)
            .count()
    }

    /// Forgets the conversation with one number.
    pub fn remove(&mut self, number: &str) {
        self.messages.retain(|message| message.number != number);
    }

    pub fn parse(text: &str) -> Self {
        let mut chat = Self::default();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let mut fields = line.splitn(5, '\t');
            let (Some(time), Some(direction), Some(number), Some(name), Some(body)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let (Ok(time), Some(incoming)) = (
                time.parse(),
                match direction {
                    "in" => Some(true),
                    "out" => Some(false),
                    _ => None,
                },
            ) else {
                continue;
            };
            if number.is_empty() {
                continue;
            }
            chat.add(ChatMessage {
                time,
                incoming,
                number: number.to_owned(),
                name: name.to_owned(),
                text: unescape(body),
            });
        }
        chat
    }

    pub fn serialize(&self) -> String {
        self.messages
            .iter()
            .map(|message| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    message.time,
                    if message.incoming { "in" } else { "out" },
                    field(&message.number),
                    field(&message.name),
                    escape(&message.text)
                )
            })
            .collect()
    }

    /// Loads the file; a missing file is an empty chat.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        crate::settings::write_through_temporary(path, &self.serialize())
    }
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => {}
            other if other.is_control() => escaped.push(' '),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            plain.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => plain.push('\n'),
            Some('t') => plain.push('\t'),
            Some(other) => plain.push(other),
            None => plain.push('\\'),
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(time: u64, incoming: bool, number: &str, text: &str) -> ChatMessage {
        ChatMessage {
            time,
            incoming,
            number: number.to_owned(),
            name: if incoming {
                "Анна".to_owned()
            } else {
                String::new()
            },
            text: text.to_owned(),
        }
    }

    #[test]
    fn conversations_are_kept_in_order_and_listed_by_the_latest() {
        let mut chat = Chat::default();
        chat.add(message(1, false, "4010", "Привет"));
        chat.add(message(2, true, "4010", "Здравствуй"));
        chat.add(message(3, true, "4011", "Кто тут?"));
        chat.add(message(4, false, "4010", "Это я"));
        let texts: Vec<&str> = chat.with("4010").iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["Привет", "Здравствуй", "Это я"]);
        let numbers: Vec<String> = chat
            .partners()
            .into_iter()
            .map(|(number, _)| number)
            .collect();
        assert_eq!(numbers, ["4010", "4011"]);
        assert_eq!(chat.received_since(1), 2);
        assert_eq!(chat.received_since(3), 0);
        chat.remove("4010");
        assert_eq!(chat.messages().len(), 1);
    }

    #[test]
    fn any_text_survives_the_file() {
        let mut chat = Chat::default();
        chat.add(message(10, true, "4010", "две\nстроки\tи таб \\ и косая"));
        chat.add(message(11, false, "+74959999999", "обычный"));
        let text = chat.serialize();
        assert_eq!(text.lines().count(), 2);
        assert_eq!(Chat::parse(&text), chat);
        // Damaged lines are skipped.
        let damaged = format!(
            "{text}x\tin\t4010\t\ttext\n12\tsideways\t4010\t\ttext\n13\tin\t\t\tno number\n"
        );
        assert_eq!(Chat::parse(&damaged), chat);
    }

    #[test]
    fn the_oldest_go_beyond_the_limit_and_the_file_round_trips() {
        let mut chat = Chat::default();
        for time in 0..(CHAT_LIMIT as u64 + 5) {
            chat.add(message(time, true, "4010", "x"));
        }
        assert_eq!(chat.messages().len(), CHAT_LIMIT);
        assert_eq!(chat.messages()[0].time, 5);

        let directory =
            std::env::temp_dir().join(format!("microsip-rs-chat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("messages.tsv");
        assert_eq!(Chat::load(&path).unwrap(), Chat::default());
        chat.save(&path).unwrap();
        assert_eq!(Chat::load(&path).unwrap(), chat);
        fs::remove_dir_all(&directory).unwrap();
    }
}
