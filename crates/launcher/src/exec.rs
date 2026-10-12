//! Tokenizer for the `.desktop` `Exec` key.
//!
//! `freedesktop-desktop-entry` ships `parse_exec`, but it splits on ASCII
//! whitespace without honouring the spec's quoting rules, so an argument like
//! `"my app"` gets torn apart. We tokenize the raw value ourselves instead,
//! applying the field-code substitutions the spec asks for.

/// Turns a raw `Exec` value into an argv, substituting field codes.
///
/// `icon`, `name` and `path` supply the values for `%i`, `%c` and `%k`. The
/// file and URL codes (`%f`, `%F`, `%u`, `%U`) and any unknown or deprecated
/// code expand to nothing, matching how they behave when an app is launched
/// without arguments.
pub fn argv(exec: &str, icon: Option<&str>, name: &str, path: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut in_quotes = false;
    let mut chars = exec.chars();

    while let Some(c) = chars.next() {
        match c {
            // A backslash escapes the next character, both inside and outside
            // quotes.
            '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                    started = true;
                }
            }
            // Quotes group whitespace but are not part of the argument. An
            // empty pair still marks an (empty) argument.
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            '%' => match chars.next() {
                Some('%') => {
                    current.push('%');
                    started = true;
                }
                Some('i') => {
                    if let Some(icon) = icon {
                        if started {
                            args.push(std::mem::take(&mut current));
                            started = false;
                        }
                        args.push("-i".to_string());
                        args.push(icon.to_string());
                    }
                }
                Some('c') => {
                    current.push_str(name);
                    started = true;
                }
                Some('k') => {
                    current.push_str(path);
                    started = true;
                }
                // File/URL codes and unknown or deprecated codes drop out.
                Some(_) => {}
                None => {
                    current.push('%');
                    started = true;
                }
            },
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }

    if started {
        args.push(current);
    }

    args
}

#[cfg(test)]
mod tests {
    use super::argv;

    fn run(exec: &str) -> Vec<String> {
        argv(exec, None, "App", "/apps/app.desktop")
    }

    #[test]
    fn expand() {
        assert_eq!(
            argv("foo %i", Some("icon"), "App", "/p"),
            ["foo", "-i", "icon"]
        );
        assert_eq!(argv("foo bar", Some("icon"), "App", "/p"), ["foo", "bar"]);
    }

    #[test]
    fn plain_command() {
        assert_eq!(run("firefox %u"), ["firefox"]);
        assert_eq!(run("code --unity-launch %F"), ["code", "--unity-launch"]);
    }

    #[test]
    fn quoted_arguments_keep_spaces() {
        assert_eq!(
            run(r#"/usr/bin/foo --name "My App" %U"#),
            ["/usr/bin/foo", "--name", "My App"]
        );
    }

    #[test]
    fn backslash_escapes() {
        assert_eq!(run(r"foo a\ b"), ["foo", "a b"]);
        assert_eq!(run(r#"foo \"quoted\""#), ["foo", "\"quoted\""]);
    }

    #[test]
    fn empty_quotes_are_an_argument() {
        assert_eq!(run(r#"foo """#), ["foo", ""]);
    }

    #[test]
    fn field_codes_do_not_leave_empty_arguments() {
        assert_eq!(run("foo %f %U %i %d"), ["foo"]);
    }

    #[test]
    fn name_path_and_percent_substitution() {
        assert_eq!(run("run %c %k"), ["run", "App", "/apps/app.desktop"]);
        assert_eq!(run("echo 100%%"), ["echo", "100%"]);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(run("   ").is_empty());
    }
}
