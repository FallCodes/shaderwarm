//! Minimal parser for Valve's text KeyValues format (`.vdf` / `.acf`).

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Child lookup by key. Steam is inconsistent about key casing, so this is case-insensitive.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(e) => e,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Open,
    Close,
    Str(String),
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Lexer<'_> {
    fn next_token(&mut self) -> Result<Option<Token>> {
        loop {
            let Some(&c) = self.chars.peek() else {
                return Ok(None);
            };
            match c {
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '/' => {
                    self.chars.next();
                    if self.chars.peek() != Some(&'/') {
                        bail!("unexpected '/' in vdf");
                    }
                    for c in self.chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                '{' => {
                    self.chars.next();
                    return Ok(Some(Token::Open));
                }
                '}' => {
                    self.chars.next();
                    return Ok(Some(Token::Close));
                }
                '"' => {
                    self.chars.next();
                    return self.quoted().map(Some);
                }
                _ => {
                    let tok = self.unquoted();
                    // Platform conditionals such as `[$WIN32]` carry no data for us.
                    if tok.starts_with('[') && tok.ends_with(']') {
                        continue;
                    }
                    return Ok(Some(Token::Str(tok)));
                }
            }
        }
    }

    fn quoted(&mut self) -> Result<Token> {
        let mut s = String::new();
        while let Some(c) = self.chars.next() {
            match c {
                '"' => return Ok(Token::Str(s)),
                '\\' => match self.chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some(other) => s.push(other),
                    None => break,
                },
                c => s.push(c),
            }
        }
        bail!("unterminated string in vdf")
    }

    fn unquoted(&mut self) -> String {
        let mut s = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
                break;
            }
            s.push(c);
            self.chars.next();
        }
        s
    }
}

/// Parses a whole document. The result is an `Obj` holding the top-level pairs.
pub fn parse(input: &str) -> Result<Value> {
    let mut lexer = Lexer { chars: input.chars().peekable() };
    Ok(Value::Obj(parse_pairs(&mut lexer, false)?))
}

fn parse_pairs(lexer: &mut Lexer, nested: bool) -> Result<Vec<(String, Value)>> {
    let mut pairs = Vec::new();
    loop {
        let key = match lexer.next_token()? {
            None if nested => bail!("unexpected end of vdf inside block"),
            None => return Ok(pairs),
            Some(Token::Close) if nested => return Ok(pairs),
            Some(Token::Close) => bail!("unbalanced '}}' in vdf"),
            Some(Token::Open) => bail!("expected key, found '{{'"),
            Some(Token::Str(k)) => k,
        };
        let value = match lexer.next_token()? {
            Some(Token::Open) => Value::Obj(parse_pairs(lexer, true)?),
            Some(Token::Str(v)) => Value::Str(v),
            _ => bail!("expected value for key '{key}'"),
        };
        pairs.push((key, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_blocks_and_escapes() {
        let doc = parse(
            r#"
            // comment
            "AppState"
            {
                "appid"     "1384160"
                "name"      "GUILTY \"GEAR\""
                "UserConfig" { "language" "english" }
            }
            "#,
        )
        .unwrap();
        let app = doc.get("appstate").unwrap();
        assert_eq!(app.get_str("APPID"), Some("1384160"));
        assert_eq!(app.get_str("name"), Some("GUILTY \"GEAR\""));
        assert_eq!(app.get("UserConfig").unwrap().get_str("language"), Some("english"));
    }

    #[test]
    fn skips_conditionals_and_unquoted_tokens() {
        let doc = parse("key value [$WIN32]\nother { a b }").unwrap();
        assert_eq!(doc.get_str("key"), Some("value"));
        assert_eq!(doc.get("other").unwrap().get_str("a"), Some("b"));
    }

    #[test]
    fn rejects_unbalanced_input() {
        assert!(parse("\"a\" { \"b\" \"c\"").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"a\"").is_err());
    }
}
