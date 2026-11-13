//! The list of recent calls, kept in a plain text file next to the settings.
//!
//! One call per line, fields separated by tabs:
//! `time  direction  seconds  number  name`. Unreadable lines are skipped, so
//! a damaged file loses those calls only.

use std::fs;
use std::io;
use std::path::Path;

/// Calls kept; the oldest are dropped beyond this.
pub const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    /// An incoming call that was answered.
    Incoming,
    /// An incoming call that was not answered.
    Missed,
}

impl Direction {
    fn code(self) -> &'static str {
        match self {
            Direction::Outgoing => "out",
            Direction::Incoming => "in",
            Direction::Missed => "missed",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "out" => Some(Direction::Outgoing),
            "in" => Some(Direction::Incoming),
            "missed" => Some(Direction::Missed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    /// Start of the call, seconds since 1970-01-01 UTC.
    pub time: u64,
    pub direction: Direction,
    /// Length of the conversation; zero when the call was not connected.
    pub seconds: u64,
    pub number: String,
    /// Caller's display name, if the network sent one.
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// Newest first.
    records: Vec<CallRecord>,
}

impl History {
    pub fn records(&self) -> &[CallRecord] {
        &self.records
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Adds a finished call as the newest entry.
    pub fn add(&mut self, record: CallRecord) {
        self.records.insert(0, record);
        self.records.truncate(HISTORY_LIMIT);
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Missed calls newer than `since` (seconds since 1970).
    pub fn missed_since(&self, since: u64) -> usize {
        self.records
            .iter()
            .filter(|record| record.direction == Direction::Missed && record.time > since)
            .count()
    }

    pub fn parse(text: &str) -> Self {
        let mut records: Vec<CallRecord> = text
            .trim_start_matches('\u{feff}')
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let time = fields.next()?.trim().parse().ok()?;
                let direction = Direction::from_code(fields.next()?.trim())?;
                let seconds = fields.next()?.trim().parse().ok()?;
                let number = fields.next()?.trim().to_owned();
                let name = fields.next().unwrap_or("").trim().to_owned();
                (!number.is_empty()).then_some(CallRecord {
                    time,
                    direction,
                    seconds,
                    number,
                    name,
                })
            })
            .collect();
        records.truncate(HISTORY_LIMIT);
        Self { records }
    }

    pub fn serialize(&self) -> String {
        self.records
            .iter()
            .map(|record| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    record.time,
                    record.direction.code(),
                    record.seconds,
                    field(&record.number),
                    field(&record.name)
                )
            })
            .collect()
    }

    /// Loads the file; a missing file is an empty history.
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

/// A field must not contain the separators of the file.
pub(crate) fn field(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(time: u64, direction: Direction, number: &str) -> CallRecord {
        CallRecord {
            time,
            direction,
            seconds: time % 100,
            number: number.to_owned(),
            name: String::new(),
        }
    }

    #[test]
    fn newest_call_is_first_and_the_list_is_limited() {
        let mut history = History::default();
        assert!(history.is_empty());
        for time in 0..(HISTORY_LIMIT as u64 + 5) {
            history.add(record(time, Direction::Outgoing, "100"));
        }
        assert_eq!(history.records().len(), HISTORY_LIMIT);
        assert_eq!(history.records()[0].time, HISTORY_LIMIT as u64 + 4);
        assert_eq!(history.records()[HISTORY_LIMIT - 1].time, 5);
        history.clear();
        assert!(history.is_empty());
    }

    #[test]
    fn saved_text_reads_back_the_same() {
        let mut history = History::default();
        history.add(record(1_794_436_200, Direction::Outgoing, "74959999999"));
        history.add(CallRecord {
            time: 1_794_436_300,
            direction: Direction::Incoming,
            seconds: 65,
            number: "4010".to_owned(),
            name: "Иван Петров".to_owned(),
        });
        history.add(record(
            1_794_436_400,
            Direction::Missed,
            "+7 (901) 075-06-27",
        ));

        let text = history.serialize();
        assert_eq!(text.lines().count(), 3);
        assert!(text.starts_with("1794436400\tmissed\t0\t+7 (901) 075-06-27\t\n"));
        assert_eq!(History::parse(&text), history);
    }

    #[test]
    fn damaged_lines_are_skipped_and_fields_cannot_break_the_file() {
        let text = "\u{feff}100\tout\t5\t4009\tАнна\n\
                    garbage\n\
                    200\tsideways\t5\t4009\t\n\
                    300\tin\tlong\t4009\t\n\
                    400\tin\t5\t\t\n\
                    500\tmissed\t0\t4010\n";
        let history = History::parse(text);
        let times: Vec<u64> = history.records().iter().map(|record| record.time).collect();
        assert_eq!(times, [100, 500]);
        assert_eq!(history.records()[0].name, "Анна");

        let mut history = History::default();
        history.add(CallRecord {
            time: 1,
            direction: Direction::Incoming,
            seconds: 2,
            number: "40\t10".to_owned(),
            name: "two\nlines".to_owned(),
        });
        let parsed = History::parse(&history.serialize());
        assert_eq!(parsed.records().len(), 1);
        assert_eq!(parsed.records()[0].number, "40 10");
        assert_eq!(parsed.records()[0].name, "two lines");
    }

    #[test]
    fn missed_calls_are_counted_after_a_given_moment() {
        let mut history = History::default();
        history.add(record(10, Direction::Missed, "1"));
        history.add(record(20, Direction::Incoming, "2"));
        history.add(record(30, Direction::Missed, "3"));
        history.add(record(40, Direction::Missed, "4"));
        assert_eq!(history.missed_since(0), 3);
        assert_eq!(history.missed_since(10), 2);
        assert_eq!(history.missed_since(40), 0);
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let directory =
            std::env::temp_dir().join(format!("microsip-rs-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("history.tsv");
        assert_eq!(History::load(&path).unwrap(), History::default());

        let mut history = History::default();
        history.add(record(7, Direction::Outgoing, "100"));
        history.save(&path).unwrap();
        assert_eq!(History::load(&path).unwrap(), history);
        fs::remove_dir_all(&directory).unwrap();
    }
}
