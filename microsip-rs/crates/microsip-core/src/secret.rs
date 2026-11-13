//! Keeping the account password between runs without writing it in the clear.
//!
//! On Windows the password is encrypted with the Data Protection API (DPAPI),
//! which ties it to the Windows user account: the stored text is useless on
//! another computer or under another user. Elsewhere there is no such
//! facility here, so nothing is stored and the password is asked for again.

/// Encrypts `secret` for the current Windows user and returns it as hex
/// text. `None` when protection is not available.
pub fn protect(secret: &str) -> Option<String> {
    platform::protect(secret.as_bytes()).map(|bytes| to_hex(&bytes))
}

/// Reverses [`protect`]. `None` when the text is not valid, was made by
/// another user or computer, or protection is not available.
pub fn unprotect(stored: &str) -> Option<String> {
    let bytes = from_hex(stored.trim())?;
    String::from_utf8(platform::unprotect(&bytes)?).ok()
}

/// True when [`protect`] can work on this system.
pub fn is_available() -> bool {
    cfg!(windows)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;

        fn CryptUnprotectData(
            data_in: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// Never show a Windows prompt; fail instead.
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    /// Copies the system's output buffer and releases it.
    fn take(output: DataBlob) -> Option<Vec<u8>> {
        if output.data.is_null() {
            return None;
        }
        // SAFETY: on success the system returned `size` readable bytes at
        // `data`, allocated with LocalAlloc; they are copied and then freed
        // exactly once.
        unsafe {
            let bytes = std::slice::from_raw_parts(output.data, output.size as usize).to_vec();
            LocalFree(output.data.cast());
            Some(bytes)
        }
    }

    fn blob(bytes: &[u8]) -> Option<DataBlob> {
        Some(DataBlob {
            size: u32::try_from(bytes.len()).ok()?,
            // The functions only read the input, despite the pointer type.
            data: bytes.as_ptr().cast_mut(),
        })
    }

    pub fn protect(bytes: &[u8]) -> Option<Vec<u8>> {
        let input = blob(bytes)?;
        let mut output = DataBlob {
            size: 0,
            data: ptr::null_mut(),
        };
        // SAFETY: `input` points at `bytes`, which outlives the call; the
        // optional arguments are null as the API allows; `output` is a valid
        // place for the result.
        let done = unsafe {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if done == 0 { None } else { take(output) }
    }

    pub fn unprotect(bytes: &[u8]) -> Option<Vec<u8>> {
        let input = blob(bytes)?;
        let mut output = DataBlob {
            size: 0,
            data: ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        let done = unsafe {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if done == 0 { None } else { take(output) }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn protect(_bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn unprotect(_bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_rejects_bad_text() {
        let bytes = [0x00, 0x7f, 0x80, 0xff, 0x10];
        assert_eq!(to_hex(&bytes), "007f80ff10");
        assert_eq!(from_hex("007f80ff10"), Some(bytes.to_vec()));
        assert_eq!(from_hex("007F80FF10"), Some(bytes.to_vec()));
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex("привет"), None);
    }

    #[test]
    fn garbage_is_never_accepted_as_a_password() {
        assert_eq!(unprotect(""), None);
        assert_eq!(unprotect("not hex"), None);
        assert_eq!(unprotect("00112233"), None);
    }

    #[cfg(windows)]
    #[test]
    fn password_round_trips_and_is_not_stored_in_the_clear() {
        let password = "Пароль-Secret-123";
        let stored = protect(password).expect("DPAPI should work for the current user");
        assert!(!stored.contains("Secret"));
        assert!(stored.len() > password.len() * 2);
        assert_eq!(unprotect(&stored).as_deref(), Some(password));

        // A damaged copy does not decrypt to something else.
        let mut damaged = stored.clone();
        let last = if damaged.ends_with('0') { "1" } else { "0" };
        damaged.replace_range(damaged.len() - 1.., last);
        assert_ne!(unprotect(&damaged).as_deref(), Some(password));

        assert_eq!(
            protect("").and_then(|empty| unprotect(&empty)).as_deref(),
            Some("")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn without_windows_nothing_is_stored() {
        assert!(!is_available());
        assert_eq!(protect("secret"), None);
    }
}
