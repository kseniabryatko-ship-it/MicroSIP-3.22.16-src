//! What becomes of a number before it is dialled: MicroSIP's Dialing
//! Prefix and Dial Plan, with its rules, so that settings copied from it
//! work the same.
//!
//! A telephone number (four characters at least, digits and `* # . - ( ) /`
//! and spaces, or starting with `+`) is first cleaned of its punctuation;
//! with a prefix set, a leading `+` is dropped and the prefix put in front.
//!
//! A dial plan is a list of patterns separated by `|`, the whole number
//! having to fit one of them, tried in order: `X` (or `x`) is any one
//! character, `.` repeats the one before any number of times, `[..]` is a
//! choice, and `<match:replacement>` replaces what `match` covers. For
//! example `<8:+7>XXXXXXXXXX` dials `89201234567` as `+79201234567`, and
//! `XXX|9XXXXXXXXXX` allows short numbers and long ones starting with 9.
//! A number that fits no pattern is not dialled, as in MicroSIP.

use regex::Regex;

/// The number to dial, or `None` when the dial plan does not allow it.
/// Numbers written with `<` and `>` are dialled as they are.
pub fn prepare(number: &str, prefix: &str, plan: &str) -> Option<String> {
    if number.contains('<') && number.contains('>') {
        return Some(number.to_owned());
    }
    let mut prepared = number.to_owned();
    if is_telephone_number(&prepared) {
        prepared = sanitize(&prepared);
        if !prefix.is_empty() && prepared.chars().count() > 3 {
            let bare = prepared.strip_prefix('+').unwrap_or(&prepared);
            prepared = format!("{prefix}{bare}");
        }
    }
    let plan = plan.trim_matches(|character| matches!(character, ' ' | '(' | ')'));
    if plan.is_empty() {
        return Some(prepared);
    }
    for token in plan.split('|').filter(|token| !token.is_empty()) {
        if let Some(result) = apply_pattern(token, &prepared) {
            return Some(result);
        }
    }
    None
}

/// MicroSIP's test for a telephone number.
fn is_telephone_number(number: &str) -> bool {
    if number.chars().count() < 4 {
        return false;
    }
    number.starts_with('+')
        || number.chars().all(|character| {
            character.is_ascii_digit()
                || matches!(character, '*' | '#' | '.' | '-' | '(' | ')' | '/' | ' ')
        })
}

/// The number without the punctuation people write it with.
fn sanitize(number: &str) -> String {
    number
        .chars()
        .filter(|character| !matches!(character, '.' | '-' | '(' | ')' | '/' | ' '))
        .collect()
}

/// One pattern of the plan: the number as changed by it when it fits.
fn apply_pattern(token: &str, number: &str) -> Option<String> {
    // The pattern as a regular expression, each `<match:replacement>` a
    // group, and the replacements in their order.
    let mut pattern = String::from("^");
    let mut replacements = Vec::new();
    let mut characters = token.chars();
    while let Some(character) = characters.next() {
        if character != '<' {
            pattern.push_str(&translate(character));
            continue;
        }
        let group: String = characters.by_ref().take_while(|c| *c != '>').collect();
        match group.split_once(':') {
            Some((matched, replacement)) => {
                pattern.push('(');
                matched
                    .chars()
                    .for_each(|c| pattern.push_str(&translate(c)));
                pattern.push(')');
                replacements.push(replacement.to_owned());
            }
            None => group.chars().for_each(|c| pattern.push_str(&translate(c))),
        }
    }
    pattern.push('$');
    // A pattern that is no valid expression is passed over.
    let captures = Regex::new(&pattern).ok()?.captures(number)?;
    let mut result = String::new();
    let mut from = 0;
    for (index, replacement) in replacements.iter().enumerate() {
        let Some(group) = captures.get(index + 1) else {
            continue;
        };
        result.push_str(&number[from..group.start()]);
        result.push_str(replacement);
        from = group.end();
    }
    result.push_str(&number[from..]);
    Some(result)
}

/// One character of a pattern in the regular expression: `X` any
/// character, `.` "the one before, any number of times", `+` as written
/// for a number; the rest as it is.
fn translate(character: char) -> String {
    match character {
        'x' | 'X' => ".".to_owned(),
        '.' => "*".to_owned(),
        '+' => "\\+".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_prefix_and_plan_numbers_are_only_cleaned() {
        assert_eq!(
            prepare("8 (920) 123-45-67", "", "").as_deref(),
            Some("89201234567")
        );
        // Short internal numbers and names are left alone.
        assert_eq!(prepare("401", "", "").as_deref(), Some("401"));
        assert_eq!(prepare("ivan@pbx", "", "").as_deref(), Some("ivan@pbx"));
    }

    #[test]
    fn the_prefix_goes_before_telephone_numbers_without_their_plus() {
        assert_eq!(
            prepare("+7 920 123 45 67", "9", "").as_deref(),
            Some("979201234567")
        );
        assert_eq!(prepare("4010", "9", "").as_deref(), Some("94010"));
        // Three characters are an extension, not a telephone number.
        assert_eq!(prepare("401", "9", "").as_deref(), Some("401"));
    }

    #[test]
    fn the_plan_replaces_what_it_marks() {
        let plan = "<8:+7>XXXXXXXXXX";
        assert_eq!(
            prepare("89201234567", "", plan).as_deref(),
            Some("+79201234567")
        );
        // Several replacements, and text kept between them.
        assert_eq!(
            prepare("123456", "", "<1:A>XX<4:B>XX").as_deref(),
            Some("A23B56")
        );
    }

    #[test]
    fn the_first_fitting_pattern_wins_and_none_fitting_blocks_the_call() {
        let plan = "(XXX|<8:7>XXXXXXXXXX|9X.)";
        assert_eq!(prepare("401", "", plan).as_deref(), Some("401"));
        assert_eq!(
            prepare("89201234567", "", plan).as_deref(),
            Some("79201234567")
        );
        assert_eq!(
            prepare("9123456789012", "", plan).as_deref(),
            Some("9123456789012")
        );
        assert_eq!(prepare("40", "", plan), None);
        assert_eq!(prepare("5555", "", plan), None);
        // Choices of characters.
        assert_eq!(prepare("4012", "", "40[0-3]X").as_deref(), Some("4012"));
        assert_eq!(prepare("4052", "", "40[0-3]X"), None);
    }

    #[test]
    fn written_with_angle_brackets_a_number_is_dialled_as_it_is() {
        assert_eq!(
            prepare("<sip:4010@pbx>", "9", "XXX").as_deref(),
            Some("<sip:4010@pbx>")
        );
    }
}
