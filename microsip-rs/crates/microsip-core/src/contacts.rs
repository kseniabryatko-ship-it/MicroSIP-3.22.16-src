//! The contact list, kept in a plain text file next to the settings: one
//! contact per line, `name<TAB>number`, and `<TAB>watch` after it for a
//! contact whose line state is shown (a busy lamp).

use crate::history::field;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub number: String,
    /// Its line state is shown next to it.
    pub watched: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contacts {
    /// Sorted by name, ignoring letter case.
    list: Vec<Contact>,
}

impl Contacts {
    pub fn list(&self) -> &[Contact] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Adds a contact, or renames the one that already has this number.
    /// Returns false when the name or the number is empty.
    pub fn set(&mut self, name: &str, number: &str) -> bool {
        let (name, number) = (field(name), field(number));
        if name.is_empty() || number.is_empty() {
            return false;
        }
        let key = digits(&number);
        // Renaming keeps the lamp as it was.
        let watched = self
            .list
            .iter()
            .any(|contact| digits(&contact.number) == key && contact.watched);
        self.list.retain(|contact| digits(&contact.number) != key);
        self.list.push(Contact {
            name,
            number,
            watched,
        });
        self.sort();
        true
    }

    /// Shows or stops showing the line state of the contact with this
    /// number; false when there is no such contact.
    pub fn set_watched(&mut self, number: &str, watched: bool) -> bool {
        let key = digits(number);
        let mut found = false;
        for contact in &mut self.list {
            if digits(&contact.number) == key {
                contact.watched = watched;
                found = true;
            }
        }
        found
    }

    /// The numbers whose line state is shown.
    pub fn watched(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|contact| contact.watched)
            .map(|contact| contact.number.clone())
            .collect()
    }

    /// Removes the contact with this number; false when there is none.
    pub fn remove(&mut self, number: &str) -> bool {
        let key = digits(number);
        let before = self.list.len();
        self.list.retain(|contact| digits(&contact.number) != key);
        self.list.len() != before
    }

    /// Name saved for a number. Punctuation is ignored, and a number written
    /// with and without the country prefix (`+7`, `8`) is the same number.
    pub fn name_of(&self, number: &str) -> Option<&str> {
        let dialled = digits(number);
        let key = national(&dialled);
        if key.is_empty() {
            return None;
        }
        self.list
            .iter()
            .find(|contact| national(&digits(&contact.number)) == key)
            .map(|contact| contact.name.as_str())
    }

    /// Contacts whose name or number contains `query`; all when it is empty.
    pub fn matching(&self, query: &str) -> Vec<&Contact> {
        let query = query.trim().to_lowercase();
        let query_digits = digits(&query);
        self.list
            .iter()
            .filter(|contact| {
                query.is_empty()
                    || contact.name.to_lowercase().contains(&query)
                    || (!query_digits.is_empty() && digits(&contact.number).contains(&query_digits))
            })
            .collect()
    }

    fn sort(&mut self) {
        self.list
            .sort_by_key(|contact| (contact.name.to_lowercase(), contact.number.clone()));
    }

    pub fn parse(text: &str) -> Self {
        let mut contacts = Self::default();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let mut fields = line.split('\t');
            if let (Some(name), Some(number)) = (fields.next(), fields.next())
                && contacts.set(name, number)
                && fields.next().map(str::trim) == Some("watch")
            {
                contacts.set_watched(number, true);
            }
        }
        contacts
    }

    pub fn serialize(&self) -> String {
        self.list
            .iter()
            .map(|contact| {
                let watch = if contact.watched { "\twatch" } else { "" };
                format!("{}\t{}{watch}\n", contact.name, contact.number)
            })
            .collect()
    }

    /// The list as a table for Excel and other programs: `name;number`
    /// rows under a heading, in UTF-8 with the mark Excel needs to see it.
    pub fn to_csv(&self) -> String {
        let mut text = String::from("\u{feff}Name;Number\r\n");
        for contact in &self.list {
            text.push_str(&csv_cell(&contact.name));
            text.push(';');
            text.push_str(&csv_cell(&contact.number));
            text.push_str("\r\n");
        }
        text
    }

    /// Adds the contacts of a table file (as exported here, by MicroSIP or
    /// by Excel) and returns how many rows were taken. The first two columns
    /// are the name and the number, in either order; a heading row and rows
    /// without a number are skipped. The file may be UTF-8 or Windows-1251.
    pub fn import_csv(&mut self, bytes: &[u8]) -> usize {
        let text = decode_text(bytes);
        let separator = csv_separator(&text);
        let mut taken = 0;
        for row in csv_rows(&text, separator) {
            let mut cells = row.iter().map(|cell| cell.trim());
            let (Some(first), Some(second)) = (cells.next(), cells.next()) else {
                continue;
            };
            let (name, number) = if looks_like_number(second) {
                (first, second)
            } else if looks_like_number(first) {
                (second, first)
            } else {
                continue;
            };
            if self.set(name, number) {
                taken += 1;
            }
        }
        taken
    }

    /// Loads the file; a missing file is an empty list.
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

pub(crate) fn csv_cell(value: &str) -> String {
    if value.contains([';', ',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// A telephone number or an extension: it has digits and no letters.
fn looks_like_number(cell: &str) -> bool {
    cell.chars().any(|c| c.is_ascii_digit()) && !cell.chars().any(char::is_alphabetic)
}

/// The separator that splits the first lines of a table into columns.
fn csv_separator(text: &str) -> char {
    let sample: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    [';', '\t', ',']
        .into_iter()
        .max_by_key(|separator| sample.matches(*separator).count())
        .filter(|separator| sample.contains(*separator))
        .unwrap_or(';')
}

/// Splits a table into rows of cells; a cell in quotes may hold the
/// separator, line breaks and doubled quotes.
fn csv_rows(text: &str, separator: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut characters = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(character) = characters.next() {
        if quoted {
            match character {
                '"' if characters.peek() == Some(&'"') => {
                    characters.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                other => cell.push(other),
            }
        } else if character == '"' && cell.is_empty() {
            quoted = true;
        } else if character == separator {
            row.push(std::mem::take(&mut cell));
        } else if character == '\n' {
            row.push(std::mem::take(&mut cell));
            rows.push(std::mem::take(&mut row));
        } else if character != '\r' {
            cell.push(character);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

/// Text of a file that is UTF-8 or, failing that, Windows-1251 (what older
/// Russian programs and Excel's plain "CSV" write).
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes
            .iter()
            .map(|byte| match *byte {
                byte @ 0..=0x7F => char::from(byte),
                0xA8 => 'Ё',
                0xB8 => 'ё',
                0xB9 => '№',
                0xA0 => ' ',
                byte @ 0xC0..=0xFF => {
                    char::from_u32(0x0410 + u32::from(byte - 0xC0)).unwrap_or('?')
                }
                _ => '?',
            })
            .collect(),
    }
}

/// What to dial for a number as people write or paste it. A telephone
/// number loses its spaces, brackets and dashes (`+7 (495) 123-45-67`
/// becomes `+74951234567`); an address (`ip:port`, `user@host`, `sip:...`)
/// or a name with letters only loses the spaces around and inside it.
pub fn dialable(text: &str) -> String {
    let text = text.trim();
    let is_address = text.contains('@')
        || text.to_ascii_lowercase().starts_with("sip:")
        || text.parse::<std::net::SocketAddr>().is_ok();
    if is_address || text.chars().any(char::is_alphabetic) {
        return text.chars().filter(|c| !c.is_whitespace()).collect();
    }
    text.chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
        .collect()
}

/// The dialable characters of a number: digits, `*` and `#`. Letters stay,
/// so a SIP user name such as `alice` is still a key.
fn digits(number: &str) -> String {
    number
        .chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, '*' | '#'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// An eleven-digit Russian number without its first digit (7 or 8), so that
/// `+7 901 ...` and `8 901 ...` compare equal. Other numbers are unchanged.
fn national(digits: &str) -> &str {
    if digits.len() == 11
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits.starts_with('7') || digits.starts_with('8'))
    {
        &digits[1..]
    } else {
        digits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Contacts {
        let mut contacts = Contacts::default();
        assert!(contacts.set("Такси", "+7 (495) 999-99-99"));
        assert!(contacts.set("анна", "4010"));
        assert!(contacts.set("Борис", "4011"));
        contacts
    }

    #[test]
    fn contacts_are_sorted_by_name_ignoring_case() {
        let contacts = sample();
        let names: Vec<&str> = contacts.list().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["анна", "Борис", "Такси"]);
    }

    #[test]
    fn same_number_is_one_contact_however_it_is_written() {
        let mut contacts = sample();
        // Saving the number again renames the contact instead of adding one.
        assert!(contacts.set("Такси Везёт", "7 495 999 99 99"));
        assert_eq!(contacts.list().len(), 3);
        assert_eq!(contacts.name_of("+74959999999"), Some("Такси Везёт"));
        // The caller's number may come with 8 or +7.
        assert_eq!(contacts.name_of("84959999999"), Some("Такси Везёт"));
        assert_eq!(contacts.name_of("8 (495) 999-99-99"), Some("Такси Везёт"));
        assert_eq!(contacts.name_of("4010"), Some("анна"));
        assert_eq!(contacts.name_of("401"), None);
        assert_eq!(contacts.name_of(""), None);
    }

    #[test]
    fn empty_name_or_number_is_refused_and_removal_reports_success() {
        let mut contacts = sample();
        assert!(!contacts.set("", "4012"));
        assert!(!contacts.set("Вера", "  "));
        assert_eq!(contacts.list().len(), 3);

        assert!(contacts.remove("40-10"));
        assert!(!contacts.remove("4010"));
        assert_eq!(contacts.list().len(), 2);
    }

    #[test]
    fn pasted_numbers_are_cleaned_for_dialling() {
        assert_eq!(dialable(" +7 (495) 123-45-67 "), "+74951234567");
        assert_eq!(dialable("8 901 075.06.27"), "89010750627");
        assert_eq!(dialable("4010"), "4010");
        assert_eq!(dialable("*72#"), "*72#");
        assert_eq!(dialable("\t4010\r\n"), "4010");
        // Addresses and names are kept as they are, apart from spaces.
        assert_eq!(dialable(" 192.168.1.10:5060 "), "192.168.1.10:5060");
        assert_eq!(dialable("alice@example.com"), "alice@example.com");
        assert_eq!(dialable("sip:100@pbx.local"), "sip:100@pbx.local");
        assert_eq!(dialable("reception desk"), "receptiondesk");
        assert_eq!(dialable(""), "");
        assert_eq!(dialable("()- "), "");
    }

    #[test]
    fn search_looks_in_names_and_numbers() {
        let contacts = sample();
        let found = |query: &str| -> Vec<&str> {
            contacts
                .matching(query)
                .iter()
                .map(|contact| contact.name.as_str())
                .collect()
        };
        assert_eq!(found(""), ["анна", "Борис", "Такси"]);
        assert_eq!(found("АНН"), ["анна"]);
        assert_eq!(found("401"), ["анна", "Борис"]);
        assert_eq!(found("999-99"), ["Такси"]);
        assert!(found("нет такого").is_empty());
    }

    #[test]
    fn saved_text_reads_back_and_bad_lines_are_skipped() {
        let contacts = sample();
        let text = contacts.serialize();
        assert_eq!(text, "анна\t4010\nБорис\t4011\nТакси\t+7 (495) 999-99-99\n");
        assert_eq!(Contacts::parse(&text), contacts);

        let parsed = Contacts::parse("\u{feff}Вера\t4012\nбез номера\n\t4013\nГлеб\t\n");
        assert_eq!(parsed.list().len(), 1);
        assert_eq!(parsed.list()[0].name, "Вера");
    }

    #[test]
    fn exported_table_reads_back() {
        let mut contacts = sample();
        assert!(contacts.set("ООО \"Ромашка\"; склад", "4012"));
        let csv = contacts.to_csv();
        assert!(csv.starts_with("\u{feff}Name;Number\r\nанна;4010\r\n"));
        assert!(csv.contains("\"ООО \"\"Ромашка\"\"; склад\";4012\r\n"));

        let mut imported = Contacts::default();
        assert_eq!(imported.import_csv(csv.as_bytes()), 4);
        assert_eq!(imported, contacts);
    }

    #[test]
    fn import_understands_other_programs_tables() {
        // Commas, the number first, a heading, empty and broken rows.
        let mut contacts = Contacts::default();
        let text = "Number,Name,Note\n4010,Анна,бухгалтерия\n\n,пусто\nтолько имя\n\
                    +7 (495) 999-99-99,\"Такси, город\",\n";
        assert_eq!(contacts.import_csv(text.as_bytes()), 2);
        assert_eq!(contacts.name_of("4010"), Some("Анна"));
        assert_eq!(contacts.name_of("84959999999"), Some("Такси, город"));

        // Tabs, as this program's own contacts file.
        let mut contacts = Contacts::default();
        assert_eq!(
            contacts.import_csv("Борис\t4011\r\nВера\t4012".as_bytes()),
            2
        );
        assert_eq!(contacts.name_of("4012"), Some("Вера"));

        // Importing again changes nothing; an existing number is renamed.
        assert_eq!(contacts.import_csv("Борис Петрович;4011".as_bytes()), 1);
        assert_eq!(contacts.list().len(), 2);
        assert_eq!(contacts.name_of("4011"), Some("Борис Петрович"));
    }

    #[test]
    fn import_reads_windows_1251_files() {
        // "Ёж №1;4010" as an old Russian program writes it.
        let bytes = [0xA8, 0xE6, 0x20, 0xB9, 0x31, b';', b'4', b'0', b'1', b'0'];
        let mut contacts = Contacts::default();
        assert_eq!(contacts.import_csv(&bytes), 1);
        assert_eq!(contacts.name_of("4010"), Some("Ёж №1"));
    }

    #[test]
    fn lamps_are_kept_in_the_file_and_through_a_rename() {
        let mut contacts = sample();
        assert!(contacts.watched().is_empty());
        assert!(contacts.set_watched("40-10", true));
        assert!(!contacts.set_watched("4099", true));
        assert_eq!(contacts.watched(), ["4010"]);
        // Renaming does not switch the lamp off.
        assert!(contacts.set("Анна Петровна", "4010"));
        assert_eq!(contacts.watched(), ["4010"]);

        let text = contacts.serialize();
        assert!(text.contains("Анна Петровна\t4010\twatch\n"), "{text}");
        assert!(text.contains("Борис\t4011\n"), "{text}");
        assert_eq!(Contacts::parse(&text), contacts);

        assert!(contacts.set_watched("4010", false));
        assert!(contacts.watched().is_empty());
        assert!(!contacts.serialize().contains("watch"));
    }

    #[test]
    fn file_round_trip_and_missing_file() {
        let directory =
            std::env::temp_dir().join(format!("microsip-rs-contacts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let path = directory.join("contacts.tsv");
        assert!(Contacts::load(&path).unwrap().is_empty());
        let contacts = sample();
        contacts.save(&path).unwrap();
        assert_eq!(Contacts::load(&path).unwrap(), contacts);
        fs::remove_dir_all(&directory).unwrap();
    }
}
