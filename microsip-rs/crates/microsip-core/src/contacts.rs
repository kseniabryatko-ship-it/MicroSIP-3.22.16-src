//! The contact list, kept in a plain text file next to the settings: one
//! contact per line, `name<TAB>number`.

use crate::history::field;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub number: String,
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
        self.list.retain(|contact| digits(&contact.number) != key);
        self.list.push(Contact { name, number });
        self.sort();
        true
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
            if let Some((name, number)) = line.split_once('\t') {
                contacts.set(name, number);
            }
        }
        contacts
    }

    pub fn serialize(&self) -> String {
        self.list
            .iter()
            .map(|contact| format!("{}\t{}\n", contact.name, contact.number))
            .collect()
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
