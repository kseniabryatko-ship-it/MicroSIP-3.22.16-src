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
    /// Record every call from the moment it is connected.
    pub record_calls: bool,
    /// Folder for recordings; empty means [`default_recordings_dir`].
    pub recordings_dir: String,
    /// Refuse incoming calls as busy, without ringing.
    pub do_not_disturb: bool,
    /// Answer incoming calls without a click, after `auto_answer_delay`.
    pub auto_answer: bool,
    /// Seconds an incoming call rings before it is answered automatically.
    pub auto_answer_delay: u32,
    /// Send every incoming call on to `forward_to` instead of ringing.
    pub forward: bool,
    pub forward_to: String,
    /// Device the ring of an incoming call plays on; empty means `speaker`.
    pub ring_speaker: String,
    /// WAV file played as the ring; empty means the built-in ring.
    pub ringtone: String,
    /// Closing the window keeps the phone running next to the clock.
    pub close_to_tray: bool,
    /// Ctrl+Alt+PageUp answers and Ctrl+Alt+PageDown hangs up, in any program.
    pub hotkeys: bool,
    pub light_theme: bool,
    /// Codecs in use, in the order of preference: names separated by
    /// commas (`PCMU,PCMA,G722,opus,G729`). Read with `Codec::parse_list`.
    pub codecs: String,
    /// How SIP travels to the server: `UDP`, `TCP` or `TLS`.
    pub transport: String,
    /// Address everything is sent through instead of the server; usually
    /// empty.
    pub proxy: String,
    /// With TLS: refuse a server whose certificate cannot be trusted.
    pub tls_verify: bool,
    /// Encryption of the calls' audio (SRTP): `off`, `optional` or
    /// `required`.
    pub media_encryption: String,
    /// STUN server asked for the public address of the audio; empty means
    /// none.
    pub stun: String,
    /// What "Почта" dials: the exchange's voicemail number.
    pub voicemail_number: String,
    /// Take the other side's voice out of the microphone (echo).
    pub echo_cancellation: bool,
    /// Lower steady background noise in the microphone.
    pub noise_suppression: bool,
    /// How keypad digits are sent: `auto`, `rfc2833`, `inband` or `info`.
    pub dtmf_method: String,
    /// Publish the user's status and watch colleagues' statuses.
    pub presence: bool,
    /// The user's status: `online`, `away` or `busy`.
    pub presence_status: String,
    /// Keep every saved account registered, not only the active one.
    pub all_accounts: bool,
    /// Language of the window: `ru` or `en`.
    pub language: String,
    /// The keypad plays its tones when pressed.
    pub key_tones: bool,
    /// An incoming call shows a small window with Answer and Decline.
    pub incoming_popup: bool,
    /// The phone's window stays above the other windows.
    pub always_on_top: bool,
    /// The band at the top shows the picture instead of the colour.
    pub header_picture: bool,
    /// The rest of MicroSIP's account fields, for the active account.
    pub options: AccountOptions,
    /// The saved accounts other than the active one (whose settings are
    /// the fields above), in the order they were added.
    pub accounts: Vec<AccountSettings>,
}

/// MicroSIP's account fields beyond server, user and password, with its
/// names in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOptions {
    /// Account Name: how the account is called in the list.
    pub label: String,
    /// Display Name: the name the people called see.
    pub display_name: String,
    /// Dialing Prefix: put in front of telephone numbers dialled.
    pub dialing_prefix: String,
    /// Dial Plan, in MicroSIP's syntax.
    pub dial_plan: String,
    /// Hide Caller ID: empty, `soft` or `hard`.
    pub hide_cid: String,
    /// Public Address: empty for automatic.
    pub public_addr: String,
    /// Register Refresh, in seconds (10 at least).
    pub register_refresh: u32,
    /// Keep-Alive, in seconds (2 at least).
    pub keep_alive: u32,
    pub ice: bool,
    pub allow_rewrite: bool,
    pub disable_session_timer: bool,
}

impl Default for AccountOptions {
    fn default() -> Self {
        Self {
            label: String::new(),
            display_name: String::new(),
            dialing_prefix: String::new(),
            dial_plan: String::new(),
            hide_cid: String::new(),
            public_addr: String::new(),
            register_refresh: 300,
            keep_alive: 15,
            ice: false,
            // MicroSIP leaves it off; this phone has always followed the
            // server's view, so a file without the line keeps doing that.
            allow_rewrite: true,
            disable_session_timer: false,
        }
    }
}

impl AccountOptions {
    /// Sets one field from a line of the file; false for an unknown key.
    /// Intervals are brought into range as MicroSIP does.
    fn set(&mut self, key: &str, value: &str) -> bool {
        let value = value.trim();
        match key {
            "label" => self.label = value.to_owned(),
            "display_name" => self.display_name = value.to_owned(),
            "dialing_prefix" => self.dialing_prefix = value.to_owned(),
            "dial_plan" => self.dial_plan = value.to_owned(),
            "hide_cid" => {
                self.hide_cid = crate::registration::HideCallerId::from_name(value)
                    .name()
                    .to_owned();
            }
            "public_addr" => self.public_addr = value.to_owned(),
            "register_refresh" => {
                self.register_refresh = match value.parse::<i64>().unwrap_or(0) {
                    seconds if seconds <= 0 => 300,
                    seconds => seconds.clamp(10, i64::from(u32::MAX)) as u32,
                }
            }
            "keep_alive" => {
                self.keep_alive = match value.parse::<i64>().unwrap_or(0) {
                    seconds if seconds <= 0 => 15,
                    seconds => seconds.clamp(2, i64::from(u32::MAX)) as u32,
                }
            }
            "ice" => self.ice = value == "1",
            "allow_rewrite" => self.allow_rewrite = value != "0",
            "disable_session_timer" => self.disable_session_timer = value == "1",
            _ => return false,
        }
        true
    }

    /// The lines of these fields, each key after `prefix`.
    fn lines(&self, prefix: &str) -> String {
        let flag = |on: bool| if on { "1" } else { "0" }.to_owned();
        let mut text = String::new();
        for (key, value) in [
            ("label", self.label.clone()),
            ("display_name", self.display_name.clone()),
            ("dialing_prefix", self.dialing_prefix.clone()),
            ("dial_plan", self.dial_plan.clone()),
            ("hide_cid", self.hide_cid.clone()),
            ("public_addr", self.public_addr.clone()),
            ("register_refresh", self.register_refresh.to_string()),
            ("keep_alive", self.keep_alive.to_string()),
            ("ice", flag(self.ice)),
            ("allow_rewrite", flag(self.allow_rewrite)),
            ("disable_session_timer", flag(self.disable_session_timer)),
        ] {
            text.push_str(&format!("{prefix}{key}={}\n", one_line(&value)));
        }
        text
    }
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
            record_calls: false,
            recordings_dir: String::new(),
            do_not_disturb: false,
            auto_answer: false,
            auto_answer_delay: 3,
            forward: false,
            forward_to: String::new(),
            ring_speaker: String::new(),
            ringtone: String::new(),
            close_to_tray: true,
            hotkeys: true,
            light_theme: false,
            codecs: crate::codec::Codec::format_list(&crate::codec::Codec::DEFAULT),
            transport: "UDP".to_owned(),
            proxy: String::new(),
            tls_verify: true,
            media_encryption: "off".to_owned(),
            stun: String::new(),
            voicemail_number: "*97".to_owned(),
            echo_cancellation: true,
            noise_suppression: true,
            dtmf_method: "auto".to_owned(),
            presence: false,
            presence_status: "online".to_owned(),
            all_accounts: false,
            language: "ru".to_owned(),
            key_tones: true,
            incoming_popup: true,
            always_on_top: false,
            header_picture: true,
            options: AccountOptions::default(),
            accounts: Vec::new(),
        }
    }
}

impl Settings {
    /// Reads settings text. Unknown keys, comments and malformed lines are
    /// skipped, and missing keys keep their defaults, so an old or hand-edited
    /// file never prevents the program from starting.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        // Other accounts by their number, in the order of the numbers.
        let mut accounts: std::collections::BTreeMap<usize, AccountSettings> =
            std::collections::BTreeMap::new();
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
            // `account.2.server=...`: a field of another saved account.
            if let Some((number, field)) = key
                .trim()
                .strip_prefix("account.")
                .and_then(|rest| rest.split_once('.'))
                && let Ok(number) = number.parse::<usize>()
            {
                // An unknown key neither makes an account nor loses one.
                let existing = accounts.remove(&number);
                let known = existing.is_some();
                let mut account = existing.unwrap_or_default();
                if account.set(field, value) || known {
                    accounts.insert(number, account);
                }
                continue;
            }
            if settings.options.set(key.trim(), &value) {
                continue;
            }
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
                "record_calls" => settings.record_calls = value == "1",
                "recordings_dir" => settings.recordings_dir = value,
                "do_not_disturb" => settings.do_not_disturb = value == "1",
                "auto_answer" => settings.auto_answer = value == "1",
                "forward" => settings.forward = value == "1",
                "forward_to" => settings.forward_to = value,
                "ring_speaker" => settings.ring_speaker = value,
                "ringtone" => settings.ringtone = value,
                "close_to_tray" => settings.close_to_tray = value != "0",
                "hotkeys" => settings.hotkeys = value != "0",
                "light_theme" => settings.light_theme = value == "1",
                "codecs" => settings.codecs = value,
                "transport" => settings.transport = value,
                "proxy" => settings.proxy = value,
                "tls_verify" => settings.tls_verify = value != "0",
                "media_encryption" => settings.media_encryption = value,
                "stun" => settings.stun = value,
                "voicemail_number" => settings.voicemail_number = value,
                "echo_cancellation" => settings.echo_cancellation = value != "0",
                "noise_suppression" => settings.noise_suppression = value != "0",
                "dtmf_method" => settings.dtmf_method = value,
                "presence" => settings.presence = value != "0",
                "presence_status" => settings.presence_status = value,
                "all_accounts" => settings.all_accounts = value != "0",
                "language" => settings.language = value,
                "key_tones" => settings.key_tones = value != "0",
                "incoming_popup" => settings.incoming_popup = value != "0",
                "always_on_top" => settings.always_on_top = value != "0",
                "header_picture" => settings.header_picture = value != "0",
                "auto_answer_delay" => {
                    if let Some(delay) = value.parse::<u32>().ok().filter(|delay| *delay <= 60) {
                        settings.auto_answer_delay = delay;
                    }
                }
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
        settings.accounts = accounts.into_values().collect();
        settings
    }

    pub fn serialize(&self) -> String {
        let mut text = String::from(HEADER);
        text.push('\n');
        let speaker_volume = self.speaker_volume.to_string();
        let microphone_volume = self.microphone_volume.to_string();
        let local_port = self.local_port.to_string();
        let save_password = if self.save_password { "1" } else { "0" }.to_owned();
        let record_calls = if self.record_calls { "1" } else { "0" }.to_owned();
        let flag = |on: bool| if on { "1" } else { "0" }.to_owned();
        let do_not_disturb = flag(self.do_not_disturb);
        let auto_answer = flag(self.auto_answer);
        let auto_answer_delay = self.auto_answer_delay.to_string();
        let forward = flag(self.forward);
        let close_to_tray = flag(self.close_to_tray);
        let hotkeys = flag(self.hotkeys);
        let light_theme = flag(self.light_theme);
        let tls_verify = flag(self.tls_verify);
        let echo_cancellation = flag(self.echo_cancellation);
        let noise_suppression = flag(self.noise_suppression);
        let presence = flag(self.presence);
        let all_accounts = flag(self.all_accounts);
        let key_tones = flag(self.key_tones);
        let incoming_popup = flag(self.incoming_popup);
        let always_on_top = flag(self.always_on_top);
        let header_picture = flag(self.header_picture);
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
            ("record_calls", &record_calls),
            ("recordings_dir", &self.recordings_dir),
            ("do_not_disturb", &do_not_disturb),
            ("auto_answer", &auto_answer),
            ("auto_answer_delay", &auto_answer_delay),
            ("forward", &forward),
            ("forward_to", &self.forward_to),
            ("ring_speaker", &self.ring_speaker),
            ("ringtone", &self.ringtone),
            ("close_to_tray", &close_to_tray),
            ("hotkeys", &hotkeys),
            ("light_theme", &light_theme),
            ("codecs", &self.codecs),
            ("transport", &self.transport),
            ("proxy", &self.proxy),
            ("tls_verify", &tls_verify),
            ("media_encryption", &self.media_encryption),
            ("stun", &self.stun),
            ("voicemail_number", &self.voicemail_number),
            ("echo_cancellation", &echo_cancellation),
            ("noise_suppression", &noise_suppression),
            ("dtmf_method", &self.dtmf_method),
            ("presence", &presence),
            ("presence_status", &self.presence_status),
            ("all_accounts", &all_accounts),
            ("language", &self.language),
            ("key_tones", &key_tones),
            ("incoming_popup", &incoming_popup),
            ("always_on_top", &always_on_top),
            ("header_picture", &header_picture),
        ] {
            text.push_str(key);
            text.push('=');
            text.push_str(&one_line(value));
            text.push('\n');
        }
        text.push_str(&self.options.lines(""));
        for (index, account) in self.accounts.iter().enumerate() {
            account.write(index + 1, &mut text);
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

/// A saved account that is not the active one. The active account's
/// settings are the top-level fields of [`Settings`]; switching swaps one
/// of these with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSettings {
    pub server: String,
    pub domain: String,
    pub user: String,
    pub auth_user: String,
    pub save_password: bool,
    /// As produced by `secret::protect`; empty when not saved.
    pub password_protected: String,
    pub transport: String,
    pub proxy: String,
    pub tls_verify: bool,
    pub media_encryption: String,
    pub stun: String,
    pub voicemail_number: String,
    /// Publish Presence.
    pub presence: bool,
    pub options: AccountOptions,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self::of(&Settings::default())
    }
}

impl AccountSettings {
    /// The account part of `settings` (its active account).
    pub fn of(settings: &Settings) -> Self {
        Self {
            server: settings.server.clone(),
            domain: settings.domain.clone(),
            user: settings.user.clone(),
            auth_user: settings.auth_user.clone(),
            save_password: settings.save_password,
            password_protected: settings.password_protected.clone(),
            transport: settings.transport.clone(),
            proxy: settings.proxy.clone(),
            tls_verify: settings.tls_verify,
            media_encryption: settings.media_encryption.clone(),
            stun: settings.stun.clone(),
            voicemail_number: settings.voicemail_number.clone(),
            presence: settings.presence,
            options: settings.options.clone(),
        }
    }

    /// Makes this the active account of `settings`.
    pub fn apply_to(&self, settings: &mut Settings) {
        settings.server = self.server.clone();
        settings.domain = self.domain.clone();
        settings.user = self.user.clone();
        settings.auth_user = self.auth_user.clone();
        settings.save_password = self.save_password;
        settings.password_protected = self.password_protected.clone();
        settings.transport = self.transport.clone();
        settings.proxy = self.proxy.clone();
        settings.tls_verify = self.tls_verify;
        settings.media_encryption = self.media_encryption.clone();
        settings.stun = self.stun.clone();
        settings.voicemail_number = self.voicemail_number.clone();
        settings.presence = self.presence;
        settings.options = self.options.clone();
    }

    /// How the account is shown in a list: its name when it has one,
    /// otherwise `4009 @ pbx.example.com`.
    pub fn title(&self) -> String {
        let label = self.options.label.trim();
        if label.is_empty() {
            account_title(&self.user, &self.server)
        } else {
            label.to_owned()
        }
    }

    /// Sets one field from a settings line; false for an unknown key.
    fn set(&mut self, key: &str, value: String) -> bool {
        match key {
            "server" => self.server = value,
            "domain" => self.domain = value,
            "user" => self.user = value,
            "auth_user" => self.auth_user = value,
            "save_password" => self.save_password = value != "0",
            "password_protected" => self.password_protected = value,
            "transport" => self.transport = value,
            "proxy" => self.proxy = value,
            "tls_verify" => self.tls_verify = value != "0",
            "media_encryption" => self.media_encryption = value,
            "stun" => self.stun = value,
            "voicemail_number" => self.voicemail_number = value,
            "presence" => self.presence = value != "0",
            other => return self.options.set(other, &value),
        }
        true
    }

    /// The lines of this account, numbered `number`.
    fn write(&self, number: usize, text: &mut String) {
        let flag = |on: bool| if on { "1" } else { "0" }.to_owned();
        // Without permission to remember it, no form of the password is written.
        let password = if self.save_password {
            self.password_protected.clone()
        } else {
            String::new()
        };
        for (key, value) in [
            ("server", self.server.clone()),
            ("domain", self.domain.clone()),
            ("user", self.user.clone()),
            ("auth_user", self.auth_user.clone()),
            ("save_password", flag(self.save_password)),
            ("password_protected", password),
            ("transport", self.transport.clone()),
            ("proxy", self.proxy.clone()),
            ("tls_verify", flag(self.tls_verify)),
            ("media_encryption", self.media_encryption.clone()),
            ("stun", self.stun.clone()),
            ("voicemail_number", self.voicemail_number.clone()),
            ("presence", flag(self.presence)),
        ] {
            text.push_str(&format!("account.{number}.{key}={}\n", one_line(&value)));
        }
        text.push_str(&self.options.lines(&format!("account.{number}.")));
    }
}

/// How an account is shown: `4009 @ pbx.example.com`, or what there is.
pub fn account_title(user: &str, server: &str) -> String {
    match (user.trim(), server.trim()) {
        ("", "") => "новый аккаунт".to_owned(),
        (user, "") => user.to_owned(),
        ("", server) => server.to_owned(),
        (user, server) => format!("{user} @ {server}"),
    }
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

/// Where recordings go unless the user chose a folder: a `MicroSIP-Rust
/// Recordings` folder in the user's Documents on Windows, in the home
/// folder elsewhere. `None` when the profile location is unknown.
pub fn default_recordings_dir() -> Option<PathBuf> {
    let variable = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        variable("USERPROFILE")?.join("Documents")
    } else {
        variable("HOME")?
    };
    Some(base.join("MicroSIP-Rust Recordings"))
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
            record_calls: true,
            recordings_dir: r"D:\Записи звонков".to_owned(),
            do_not_disturb: true,
            auto_answer: true,
            auto_answer_delay: 5,
            forward: true,
            forward_to: "4011".to_owned(),
            ring_speaker: "Speakers (Realtek)".to_owned(),
            ringtone: r"C:\Sounds\ring.wav".to_owned(),
            close_to_tray: false,
            hotkeys: false,
            light_theme: true,
            codecs: "opus,G722,PCMA".to_owned(),
            transport: "TLS".to_owned(),
            proxy: "proxy.example.com:5061".to_owned(),
            tls_verify: false,
            media_encryption: "required".to_owned(),
            stun: "stun.example.com:3478".to_owned(),
            voicemail_number: "*98".to_owned(),
            echo_cancellation: false,
            noise_suppression: true,
            dtmf_method: "info".to_owned(),
            presence: true,
            presence_status: "away".to_owned(),
            all_accounts: true,
            language: "en".to_owned(),
            key_tones: false,
            incoming_popup: false,
            always_on_top: true,
            header_picture: false,
            options: AccountOptions {
                label: "Офис".to_owned(),
                display_name: "Алиса Иванова".to_owned(),
                dialing_prefix: "9".to_owned(),
                dial_plan: "<8:+7>XXXXXXXXXX".to_owned(),
                hide_cid: "soft".to_owned(),
                public_addr: "192.168.1.5".to_owned(),
                register_refresh: 600,
                keep_alive: 30,
                ice: false,
                allow_rewrite: false,
                disable_session_timer: true,
            },
            accounts: Vec::new(),
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
    fn other_accounts_are_kept_numbered_and_switching_swaps_them() {
        let mut settings = sample();
        let mut office = AccountSettings::of(&settings);
        office.server = "office.example.com".to_owned();
        office.user = "4010".to_owned();
        office.password_protected = "ffee".to_owned();
        let mut home = AccountSettings {
            server: "sip.provider.ru".to_owned(),
            user: "74951234567".to_owned(),
            save_password: false,
            password_protected: "must-not-be-written".to_owned(),
            ..AccountSettings::default()
        };
        settings.accounts = vec![office.clone(), home.clone()];

        let text = settings.serialize();
        // The active account is written as always, the others after it.
        assert!(text.contains("\nserver=pbx.example.com:5080\n"));
        assert!(text.contains("\naccount.1.server=office.example.com\n"));
        assert!(text.contains("\naccount.1.password_protected=ffee\n"));
        assert!(text.contains("\naccount.2.user=74951234567\n"));
        // Not allowed to remember it: no form of that password is written.
        assert!(!text.contains("must-not-be-written"));
        assert!(text.contains("\naccount.2.password_protected=\n"));

        let parsed = Settings::parse(&text);
        home.password_protected.clear();
        assert_eq!(parsed.accounts, [office.clone(), home]);
        assert_eq!(parsed.server, "pbx.example.com:5080");

        // Switching: the chosen one becomes active, the active one is kept.
        let mut switched = parsed.clone();
        let chosen = switched.accounts.remove(0);
        let previous = AccountSettings::of(&switched);
        chosen.apply_to(&mut switched);
        switched.accounts.push(previous.clone());
        assert_eq!(switched.server, "office.example.com");
        assert_eq!(switched.user, "4010");
        assert_eq!(switched.password_protected, "ffee");
        assert_eq!(AccountSettings::of(&switched), office);
        assert_eq!(switched.accounts[1], previous);
        assert_eq!(Settings::parse(&switched.serialize()), switched);
    }

    #[test]
    fn account_lines_out_of_order_or_unknown_are_handled() {
        let text = "server=main\naccount.7.server=seven\naccount.2.server=two\n\
                    account.2.colour=blue\naccount.x.server=bad\naccount.3.nonsense=1\n";
        let settings = Settings::parse(text);
        let servers: Vec<&str> = settings
            .accounts
            .iter()
            .map(|account| account.server.as_str())
            .collect();
        // In the order of their numbers; nothing made of unknown keys.
        assert_eq!(servers, ["two", "seven"]);
        assert_eq!(settings.server, "main");
    }

    #[test]
    fn accounts_have_readable_titles() {
        assert_eq!(
            account_title("4009", "pbx.example.com"),
            "4009 @ pbx.example.com"
        );
        assert_eq!(account_title(" 4009 ", ""), "4009");
        assert_eq!(account_title("", "pbx"), "pbx");
        assert_eq!(account_title("", ""), "новый аккаунт");
        let account = AccountSettings {
            user: "100".to_owned(),
            server: "sip.local".to_owned(),
            ..AccountSettings::default()
        };
        assert_eq!(account.title(), "100 @ sip.local");
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
             local_port=5062\nsave_password=1\npassword_protected=0a1b2c\n\
             record_calls=1\nrecordings_dir=D:\\Записи звонков\n\
             do_not_disturb=1\nauto_answer=1\nauto_answer_delay=5\n\
             forward=1\nforward_to=4011\n\
             ring_speaker=Speakers (Realtek)\nringtone=C:\\Sounds\\ring.wav\n\
             close_to_tray=0\nhotkeys=0\nlight_theme=1\ncodecs=opus,G722,PCMA\n\
             transport=TLS\nproxy=proxy.example.com:5061\ntls_verify=0\n\
             media_encryption=required\nstun=stun.example.com:3478\n\
             voicemail_number=*98\necho_cancellation=0\nnoise_suppression=1\n\
             dtmf_method=info\npresence=1\npresence_status=away\nall_accounts=1\nlanguage=en\n\
             key_tones=0\nincoming_popup=0\nalways_on_top=1\n\
             header_picture=0\n\
             label=Офис\ndisplay_name=Алиса Иванова\ndialing_prefix=9\n\
             dial_plan=<8:+7>XXXXXXXXXX\nhide_cid=soft\npublic_addr=192.168.1.5\n\
             register_refresh=600\nkeep_alive=30\nice=0\nallow_rewrite=0\n\
             disable_session_timer=1\n"
        );
        // No line holds the password itself; `save_password` and
        // `password_protected` are different keys.
        assert!(text.lines().all(|line| !line.starts_with("password=")));
    }

    #[test]
    fn account_fields_of_microsip_are_kept_and_brought_into_range() {
        let settings = Settings::parse(
            "register_refresh=5\nkeep_alive=1\nhide_cid=1\naccount.1.server=two\n\
             account.1.register_refresh=0\naccount.1.keep_alive=-3\naccount.1.label=Дом\n",
        );
        // As MicroSIP: at least 10 s and 2 s; nothing usable is the default;
        // the old `1` of Hide Caller ID is hard.
        assert_eq!(settings.options.register_refresh, 10);
        assert_eq!(settings.options.keep_alive, 2);
        assert_eq!(settings.options.hide_cid, "hard");
        assert_eq!(settings.accounts[0].options.register_refresh, 300);
        assert_eq!(settings.accounts[0].options.keep_alive, 15);
        assert_eq!(settings.accounts[0].title(), "Дом");
        // A file of an earlier version keeps following the server's view.
        assert!(Settings::parse("").options.allow_rewrite);
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
        // Forwarding is off and has no number.
        assert!(!old.forward);
        assert_eq!(old.forward_to, "");
        // Do-not-disturb and auto-answer are off, with a sensible delay.
        assert!(!old.do_not_disturb);
        assert!(!old.auto_answer);
        assert_eq!(old.auto_answer_delay, 3);
        assert_eq!(
            Settings::parse("auto_answer_delay=900\n").auto_answer_delay,
            3
        );
        // Recording is off unless it was switched on.
        assert!(!old.record_calls);
        assert_eq!(old.recordings_dir, "");
        if let Some(folder) = default_recordings_dir() {
            assert!(folder.ends_with("MicroSIP-Rust Recordings"));
        }
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
