/// Quotes one word for POSIX sh. Plain words stay bare so generated scripts remain readable.
pub fn sh_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'_' | b'/' | b'.' | b':' | b'=' | b'@' | b'%' | b'+' | b',' | b'-'
                )
        });
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn leaves_plain_words_bare() {
        assert_eq!(sh_quote("--config-file"), "--config-file");
        assert_eq!(sh_quote("/usr/bin/cuprated"), "/usr/bin/cuprated");
    }

    #[test]
    fn quotes_everything_a_shell_would_interpret() {
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("$(id)"), "'$(id)'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("~root"), "'~root'");
        assert_eq!(sh_quote("*"), "'*'");
    }
}
