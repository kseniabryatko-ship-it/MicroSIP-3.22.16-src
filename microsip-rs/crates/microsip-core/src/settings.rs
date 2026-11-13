//! Settings the interface remembers between runs.
//!
//! A small `key=value` text file in the user's profile, outside the source
//! tree. The password is never written in the clear: it is either left out,
//! or stored encrypted for the current Windows user (see `secret`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const HEADER: &str =
    "# MicroSIP Rust settings. The password is kept only encrypted for this Windows user.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Local SIP address, `IP:port`.
    pub local: String,
    /// Last dialled address, `IP:port`.
    pub remote: String,
    pub server: String,
    pub domain: String,
    pub user: String,
    /// Login for authorization when it differs from `user`; usually empty.
    pub auth_user: String,
    /// Chosen microphone; empty means the system default.
    pub microphone: String,
    /// Chosen speaker or headphones; empty means the system default.
    pub speaker: String,
    /// Volumes in percent, 0 to 200; 100 is unchanged.
    pub speaker_volume: u32,
    pub microphone_volume: u32,
    /// Local SIP port; 0 lets the system pick a free one at every start.
    pub local_port: u16,
    /// Whether the password is remembered (encrypted) between runs.
    pub save_password: bool,
    /// The password as produced by `secret::protect`; empty when not saved.
    pub password_protected: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            local: "127.0.0.1:5070".to_owned(),
            remote: "127.0.0.1:5072".to_owned(),
            server: String::new(),
            domain: String::new(),
            user: String::new(),
            auth_user: String::new(),
            microphone: String::new(),
            speaker: String::new(),
            speaker_volume: 100,
            microphone_volume: 100,
            local_port: 0,
            save_password: true,
            password_protected: String::new(),
        }
    }
}

impl Settings {
    /// Reads settings text. Unknown keys, comments and malformed lines are
    /// skipped, and missing keys keep their defaults, so an old or hand-edited
    /// file never prevents the program from starting.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        // Notepad may put a byte-order mark in front of the first line.
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_owned();
            match key.trim() {
                "local" => settings.local = value,
                "remote" => settings.remote = value,
                "server" => settings.server = value,
                "domain" => settings.domain = value,
                "user" => settings.user = value,
                "auth_user" => settings.auth_user = value,
                "local_port" => {
                    if let Ok(port) = value.parse() {
                        settings.local_port = port;
                    }
                }
                "save_password" => settings.save_password = value != "0",
                "password_protected" => settings.password_protected = value,
                "microphone" => settings.microphone = value,
                "speaker" => settings.speaker = value,
                // A value that is not a sensible volume keeps the default.
                "speaker_volume" => {
                    if let Some(volume) = parse_volume(&value) {
                        settings.speaker_volume = volume;
                    }
                }
                "microphone_volume" => {
                    if let Some(volume) = parse_volume(&value) {
                        settings.microphone_volume = volume;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn serialize(&self) -> String {
        let mut text = String::from(HEADER);
        text.push('\n');
        let speaker_volume = self.speaker_volume.to_string();
        let microphone_volume = self.microphone_volume.to_string();
        let local_port = self.local_port.to_string();
        let save_password = if self.save_password { "1" } else { "0" }.to_owned();
        // Without permission to remember it, no form of the password is written.
        let password_protected = if self.save_password {
            self.password_protected.clone()
        } else {
            String::new()
        };
        for (key, value) in [
            ("local", &self.local),
            ("remote", &self.remote),
            ("server", &self.server),
            ("domain", &self.domain),
            ("user", &self.user),
            ("auth_user", &self.auth_user),
            ("microphone", &self.microphone),
            ("speaker", &self.speaker),
            ("speaker_volume", &speaker_volume),
            ("microphone_volume", &microphone_volume),
            ("local_port", &local_port),
            ("save_password", &save_password),
            ("password_protected", &password_protected),
        ] {
            text.push_str(key);
            text.push('=');
            text.push_str(&one_line(value));
            text.push('\n');
        }
        text
    }

    /// Loads the file; a missing file gives the defaults.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Writes the file through a temporary one, so a crash while saving does
    /// not leave a half-written settings file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_through_temporary(path, &self.serialize())
    }
}

/// Writes `text` to `path` through a temporary file, creating the folder.
pub(crate) fn write_through_temporary(path: &Path, text: &str) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path)
}

fn parse_volume(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|volume| *volume <= 200)
}

/// A value must stay on its line; control characters are dropped.
fn one_line(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// `%APPDATA%\MicroSIP-Rust\settings.ini` on Windows; elsewhere the
/// `MicroSIP-Rust` folder under `$XDG_CONFIG_HOME` or `$HOME/.config`.
/// `None` when the profile location is unknown.
pub fn default_path() -> Option<PathBuf> {
    let variable = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        variable("APPDATA")?
    } else {
        variable("XDG_CONFIG_HOME").or_else(|| Some(variable("HOME")?.join(".config")))?
    };
    Some(base.join("MicroSIP-Rust").join("settings.ini"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn sample() -> Settings {
        Settings {
            local: "192.168.1.5:5060".to_owned(),
            remote: "192.168.1.9:5060".to_owned(),
            server: "pbx.example.com:5080".to_owned(),
            domain: "example.com".to_owned(),
            user: "alice".to_owned(),
            auth_user: "alice-login".to_owned(),
            microphone: "Headset Microphone (USB Audio)".to_owned(),
            speaker: "Headset Earphone (USB Audio)".to_owned(),
            speaker_volume: 80,
            microphone_volume: 150,
            local_port: 5062,
            save_password: true,
            password_protected: "0a1b2c".to_owned(),
        }
    }

    /// A fresh directory under the system temporary folder.
    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "microsip-rs-settings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn serialized_settings_parse_back() {
        let settings = sample();
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn serialized_text_has_one_line_per_key_and_no_password_key() {
        let text = sample().serialize();
        assert_eq!(
            text,
            "# MicroSIP Rust settings. The password is kept only encrypted for this Windows user.\n\
             local=192.168.1.5:5060\nremote=192.168.1.9:5060\n\
             server=pbx.example.com:5080\ndomain=example.com\nuser=alice\n\
             auth_user=alice-login\n\
             microphone=Headset Microphone (USB Audio)\n\
             speaker=Headset Earphone (USB Audio)\n\
             speaker_volume=80\nmicrophone_volume=150\n\
             local_port=5062\nsave_password=1\npassword_protected=0a1b2c\n"
        );
        // No line holds the password itself; `save_password` and
        // `password_protected` are different keys.
        assert!(text.lines().all(|line| !line.starts_with("password=")));
    }

    #[test]
    fn missing_keys_keep_defaults_and_unknown_lines_are_skipped() {
        let text = "\u{feff}local=10.0.0.1:5060\r\n# comment\r\n\r\n  user = bob  \r\nserver=pbx\r\n\
                    password=must-be-ignored\r\nnot a setting\r\n=novalue\r\n";
        let settings = Settings::parse(text);
        assert_eq!(settings.user, "bob");
        assert_eq!(settings.server, "pbx");
        assert_eq!(settings.local, "10.0.0.1:5060");
        assert_eq!(settings.remote, Settings::default().remote);
        assert_eq!(settings.domain, "");
        // A file written before these settings existed has no such lines.
        assert_eq!(settings.auth_user, "");
        assert_eq!(settings.microphone, "");
        assert_eq!(settings.speaker_volume, 100);
        assert_eq!(settings.microphone_volume, 100);
    }

    #[test]
    fn value_may_contain_an_equals_sign_but_not_a_line_break() {
        let mut settings = sample();
        settings.user = "a=b".to_owned();
        settings.domain = " example.com\r\nuser=mallory ".to_owned();
        let parsed = Settings::parse(&settings.serialize());
        assert_eq!(parsed.user, "a=b");
        assert_eq!(parsed.domain, "example.comuser=mallory");
    }

    #[test]
    fn unusable_volume_values_keep_the_default() {
        let settings = Settings::parse(
            "speaker_volume=loud\nmicrophone_volume=201\nspeaker=Speakers = main\n",
        );
        assert_eq!(settings.speaker_volume, 100);
        assert_eq!(settings.microphone_volume, 100);
        // A device name may contain anything but a line break.
        assert_eq!(settings.speaker, "Speakers = main");

        let settings = Settings::parse("speaker_volume=0\nmicrophone_volume=200\n");
        assert_eq!(settings.speaker_volume, 0);
        assert_eq!(settings.microphone_volume, 200);
    }

    #[test]
    fn password_is_written_only_encrypted_and_only_when_allowed() {
        let mut settings = sample();
        settings.save_password = false;
        let text = settings.serialize();
        assert!(
            text.contains("save_password=0\npassword_protected=\n"),
            "{text}"
        );
        assert!(!text.contains("0a1b2c"));
        let parsed = Settings::parse(&text);
        assert!(!parsed.save_password);
        assert_eq!(parsed.password_protected, "");

        // Old files have none of these lines.
        let old = Settings::parse("user=bob\n");
        assert!(old.save_password);
        assert_eq!(old.password_protected, "");
        assert_eq!(old.local_port, 0);
        // A port that is not a port keeps the automatic choice.
        assert_eq!(Settings::parse("local_port=70000\n").local_port, 0);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let path = scratch().join("settings.ini");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
    }

    #[test]
    fn save_creates_the_folder_and_load_reads_it_back() {
        let directory = scratch();
        let path = directory.join("nested").join("settings.ini");
        let settings = sample();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);

        // Saving again replaces the file and leaves no temporary file behind.
        let mut changed = settings.clone();
        changed.user = "bob".to_owned();
        changed.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), changed);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.ini"]);
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn default_path_ends_with_the_program_folder_and_file() {
        if let Some(path) = default_path() {
            assert!(path.ends_with(Path::new("MicroSIP-Rust").join("settings.ini")));
        }
    }
}
