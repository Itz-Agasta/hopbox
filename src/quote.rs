//! The only place hopbox builds strings for a remote shell.
//!
//! Every dynamic value goes through [`shell_quote`]; the fixed parts of each script are literals.

use std::borrow::Cow;

/// Quote `s` as one POSIX `sh` word.
///
/// Leaves `s` bare when it is non-empty and made only of characters `sh` never treats specially.
/// Otherwise wraps it in single quotes, writing each `'` as `'\''`.
pub fn shell_quote(s: &str) -> Cow<'_, str> {
    let is_safe = |b: u8| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b);
    if !s.is_empty() && s.bytes().all(is_safe) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(format!("'{}'", s.replace('\'', r"'\''")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::process::Command;

    #[test]
    fn safe_words_stay_bare() {
        assert_eq!(shell_quote("cargo"), "cargo");
        assert_eq!(shell_quote("a/b-c.d"), "a/b-c.d");
        assert_eq!(shell_quote("--flag=v,w:x@y%z+1"), "--flag=v,w:x@y%z+1");
    }

    #[test]
    fn empty_is_quoted() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn specials_are_single_quoted() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("~"), "'~'");
        assert_eq!(shell_quote("*"), "'*'");
        assert_eq!(shell_quote("naïve"), "'naïve'");
    }

    fn sh_echo(word: &str) -> String {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {word}"))
            .output()
            .expect("run sh");
        assert!(out.status.success(), "sh failed for {word:?}");
        String::from_utf8(out.stdout).expect("utf8")
    }

    #[test]
    fn hostile_strings_round_trip() {
        for s in [
            "'", "''", "\\", "\n", "a\nb", "`id`", "$(id)", "\"", "-n", "%s",
        ] {
            assert_eq!(sh_echo(&shell_quote(s)), s, "{s:?}");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn round_trips_through_sh(s in "[^\u{0}]*") {
            prop_assert_eq!(sh_echo(&shell_quote(&s)), s);
        }
    }
}
