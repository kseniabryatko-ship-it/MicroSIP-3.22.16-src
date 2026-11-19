//! The list of recent calls, kept in a plain text file next to the settings.
//!
//! One call per line, fields separated by tabs:
//! `time  direction  seconds  number  name  recording  result`. The last
//! fields (the recording's file path, why the call did not take place) may
//! be missing. Unreadable lines are skipped, so a damaged file loses those
//! calls only.

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
    /// Path of the call's recording; empty when it was not recorded.
    pub recording: String,
    /// Why the call did not go as usual: a SIP status (`486`),
    /// `cancelled`, `timeout`, `failed`, `declined`, `dnd`, or
    /// `forwarded NUMBER`. Empty for a call that went normally.
    pub result: String,
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
                let recording = fields.next().unwrap_or("").trim().to_owned();
                let result = fields.next().unwrap_or("").trim().to_owned();
                (!number.is_empty()).then_some(CallRecord {
                    time,
                    direction,
                    seconds,
                    number,
                    name,
                    recording,
                    result,
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
                // The result only when there is one: files of calls that
                // went normally look as they always did.
                let result = if record.result.trim().is_empty() {
                    String::new()
                } else {
                    format!("\t{}", field(&record.result))
                };
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}{result}\n",
                    record.time,
                    record.direction.code(),
                    record.seconds,
                    field(&record.number),
                    field(&record.name),
                    field(&record.recording)
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

/// How a call went, as a table of calls sums it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// There was a conversation.
    Answered,
    /// Nobody talked: not answered, busy, declined, cancelled, missed.
    NotAnswered,
    /// The call could not be made: an error of the phone or the exchange.
    Failed,
}

impl CallRecord {
    pub fn outcome(&self) -> Outcome {
        let result = self.result.trim();
        if self.seconds > 0 {
            return Outcome::Answered;
        }
        if result.is_empty() {
            return match self.direction {
                Direction::Missed => Outcome::NotAnswered,
                // Connected, if only for a moment.
                Direction::Outgoing | Direction::Incoming => Outcome::Answered,
            };
        }
        let not_answered = matches!(result, "cancelled" | "timeout" | "declined" | "dnd")
            || result.starts_with("forwarded")
            // Busy, unavailable, no answer, declined, ended before an answer.
            || matches!(
                result.parse::<u16>(),
                Ok(408 | 480 | 486 | 487 | 600 | 603)
            );
        if not_answered {
            Outcome::NotAnswered
        } else {
            Outcome::Failed
        }
    }
}

/// The words of a call table, in the window's language.
pub struct TableWords<'a> {
    /// Date, time, direction, number, name, length, status, reason.
    pub headings: [&'a str; 8],
    /// Outgoing, incoming, missed.
    pub directions: [&'a str; 3],
    /// Answered, not answered, failed.
    pub outcomes: [&'a str; 3],
    /// What a result code means, in words; `None` for none.
    pub reason: &'a dyn Fn(&str) -> Option<String>,
}

impl History {
    /// The whole list as a table for Excel: newest call first, in UTF-8
    /// with the mark Excel needs, `;` between columns. The date and the
    /// time of day are local (`offset` seconds from UTC); the length is
    /// `H:MM:SS` and empty without a conversation. Numbers are written so
    /// that Excel keeps them as text (no 7,92E+10, no lost leading zero).
    pub fn to_csv(&self, offset: i64, words: &TableWords<'_>) -> String {
        use crate::contacts::csv_cell;
        let mut text = String::from("\u{feff}");
        text.push_str(&words.headings.map(csv_cell).join(";"));
        text.push_str("\r\n");
        for record in &self.records {
            let time = crate::localtime::DateTime::from_unix(record.time, offset);
            let direction = match record.direction {
                Direction::Outgoing => words.directions[0],
                Direction::Incoming => words.directions[1],
                Direction::Missed => words.directions[2],
            };
            let length = if record.seconds > 0 {
                let seconds = record.seconds;
                format!(
                    "{}:{:02}:{:02}",
                    seconds / 3_600,
                    seconds % 3_600 / 60,
                    seconds % 60
                )
            } else {
                String::new()
            };
            let outcome = match record.outcome() {
                Outcome::Answered => words.outcomes[0],
                Outcome::NotAnswered => words.outcomes[1],
                Outcome::Failed => words.outcomes[2],
            };
            let number = if record
                .number
                .chars()
                .all(|c| c.is_ascii_digit() || c == '+')
                && !record.number.is_empty()
            {
                format!("=\"{}\"", record.number)
            } else {
                csv_cell(&record.number)
            };
            let cells = [
                format!("{:02}.{:02}.{}", time.day, time.month, time.year),
                format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second),
                csv_cell(direction),
                number,
                csv_cell(&record.name),
                length,
                csv_cell(outcome),
                csv_cell(&(words.reason)(&record.result).unwrap_or_default()),
            ];
            text.push_str(&cells.join(";"));
            text.push_str("\r\n");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(direction: Direction, seconds: u64, result: &str) -> CallRecord {
        CallRecord {
            time: 1_795_060_000,
            direction,
            seconds,
            number: "79203181933".to_owned(),
            name: String::new(),
            recording: String::new(),
            result: result.to_owned(),
        }
    }

    #[test]
    fn each_call_is_answered_not_answered_or_failed() {
        use Direction::{Incoming, Missed, Outgoing};
        for (record, outcome) in [
            (call(Outgoing, 65, ""), Outcome::Answered),
            (call(Incoming, 0, ""), Outcome::Answered),
            (call(Missed, 0, ""), Outcome::NotAnswered),
            (call(Outgoing, 0, "486"), Outcome::NotAnswered),
            (call(Outgoing, 0, "cancelled"), Outcome::NotAnswered),
            (call(Outgoing, 0, "timeout"), Outcome::NotAnswered),
            (call(Missed, 0, "dnd"), Outcome::NotAnswered),
            (call(Missed, 0, "forwarded 4011"), Outcome::NotAnswered),
            (call(Outgoing, 0, "503"), Outcome::Failed),
            (call(Outgoing, 0, "404"), Outcome::Failed),
            (call(Outgoing, 0, "failed"), Outcome::Failed),
        ] {
            assert_eq!(record.outcome(), outcome, "{record:?}");
        }
    }

    #[test]
    fn the_call_table_opens_in_excel_with_every_column() {
        let mut history = History::default();
        history.add(call(Direction::Outgoing, 0, "503"));
        let mut talked = call(Direction::Incoming, 3_725, "");
        talked.time += 60;
        talked.number = "4010".to_owned();
        talked.name = "Анна; бухгалтерия".to_owned();
        history.add(talked);
        let reason = |code: &str| (code == "503").then(|| "сервис недоступен (503)".to_owned());
        let words = TableWords {
            headings: [
                "Дата",
                "Время",
                "Направление",
                "Номер",
                "Имя",
                "Длительность",
                "Статус",
                "Причина",
            ],
            directions: ["Исходящий", "Входящий", "Пропущенный"],
            outcomes: ["Дозвон", "Недозвон", "Ошибка"],
            reason: &reason,
        };
        // Moscow time, three hours from UTC.
        let table = history.to_csv(3 * 3_600, &words);
        let lines: Vec<&str> = table.split("\r\n").collect();
        assert_eq!(
            lines[0],
            "\u{feff}Дата;Время;Направление;Номер;Имя;Длительность;Статус;Причина"
        );
        // Newest first; a name with the separator is quoted.
        assert_eq!(
            lines[1],
            "19.11.2026;06:47:40;Входящий;=\"4010\";\"Анна; бухгалтерия\";1:02:05;Дозвон;"
        );
        assert_eq!(
            lines[2],
            "19.11.2026;06:46:40;Исходящий;=\"79203181933\";;;Ошибка;сервис недоступен (503)"
        );
        assert_eq!(lines.len(), 4);
    }

    fn record(time: u64, direction: Direction, number: &str) -> CallRecord {
        CallRecord {
            time,
            direction,
            seconds: time % 100,
            number: number.to_owned(),
            name: String::new(),
            recording: String::new(),
            result: String::new(),
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
            recording: r"C:\Users\kseni\Documents\calls\2026-11-12_01-31-40_in_4010.wav".to_owned(),
            result: String::new(),
        });
        history.add(record(
            1_794_436_400,
            Direction::Missed,
            "+7 (901) 075-06-27",
        ));

        let text = history.serialize();
        assert_eq!(text.lines().count(), 3);
        assert!(text.starts_with("1794436400\tmissed\t0\t+7 (901) 075-06-27\t\t\n"));
        assert!(text.contains("\tИван Петров\tC:\\Users\\kseni\\Documents\\calls\\"));
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
        // Lines written before recordings existed have no such field.
        assert_eq!(history.records()[0].recording, "");
        assert_eq!(history.records()[1].recording, "");

        let mut history = History::default();
        history.add(CallRecord {
            time: 1,
            direction: Direction::Incoming,
            seconds: 2,
            number: "40\t10".to_owned(),
            name: "two\nlines".to_owned(),
            recording: String::new(),
            result: String::new(),
        });
        let parsed = History::parse(&history.serialize());
        assert_eq!(parsed.records().len(), 1);
        assert_eq!(parsed.records()[0].number, "40 10");
        assert_eq!(parsed.records()[0].name, "two lines");
    }

    #[test]
    fn why_a_call_failed_is_kept_and_old_lines_have_no_result() {
        let mut history = History::default();
        history.add(CallRecord {
            result: "486".to_owned(),
            ..record(100, Direction::Outgoing, "4010")
        });
        history.add(CallRecord {
            result: "forwarded 4011".to_owned(),
            ..record(200, Direction::Missed, "4012")
        });
        history.add(record(300, Direction::Incoming, "4013"));
        let text = history.serialize();
        assert!(text.contains("100\tout\t0\t4010\t\t\t486\n"), "{text}");
        assert!(text.contains("\t\tforwarded 4011\n"), "{text}");
        // A call that went normally is written as before.
        assert!(text.starts_with("300\tin\t0\t4013\t\t\n"), "{text}");
        assert_eq!(History::parse(&text), history);
        // A line of an older version, with six fields, has no result.
        let old = History::parse("1\tout\t3\t4010\tАнна\tC:\\rec.wav\n");
        assert_eq!(old.records()[0].result, "");
        assert_eq!(old.records()[0].recording, "C:\\rec.wav");
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
