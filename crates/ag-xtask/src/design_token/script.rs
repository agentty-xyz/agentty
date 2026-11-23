//! Raw colour detection in page scripts, whose colours must come from design
//! token custom properties read at runtime.
//!
//! Only the text of string and template literals is searched, so comments,
//! identifiers such as private `#fields`, and regular expressions never
//! match. A literal holds a raw colour when it holds a hex colour or a colour
//! function with literal channels, such as `rgba(0, 0, 0, 0.2)`; a function
//! whose channels the script computes, as in `'rgb(' + channel`, writes no
//! value down. A string passed straight to a selector method, such as
//! `querySelector('#add')`, is a selector. Named colours are not searched,
//! because scripts use colour words as data, such as the `green` theme name.

use std::mem;

use super::stylesheet::{COLOR_FUNCTIONS, RawColor};

/// Methods whose first argument is a CSS selector, where `#` starts an id.
const SELECTOR_METHODS: [&str; 4] = ["closest", "matches", "querySelector", "querySelectorAll"];

/// Keywords after which `/` starts a regular expression, not a division.
const REGEX_KEYWORDS: [&str; 14] = [
    "await",
    "case",
    "delete",
    "do",
    "else",
    "in",
    "instanceof",
    "new",
    "of",
    "return",
    "throw",
    "typeof",
    "void",
    "yield",
];

/// Returns every raw colour in the literals of `source`, a script whose
/// first line is line `first_line` of its file.
pub(super) fn raw_colors(source: &str, first_line: usize) -> Vec<RawColor> {
    let mut lexer = Lexer {
        before_previous: Token::None,
        chars: source.chars().collect(),
        found: Vec::new(),
        index: 0,
        line: first_line,
        previous: Token::None,
    };
    lexer.code(false);

    lexer.found
}

/// What the lexer last read, which decides whether `/` starts a regular
/// expression and whether a string is a selector argument.
#[derive(PartialEq)]
enum Token {
    None,
    Punct(char),
    Value,
    Word(String),
}

/// Single-pass lexer that scans literals as it meets them.
struct Lexer {
    before_previous: Token,
    chars: Vec<char>,
    found: Vec<RawColor>,
    index: usize,
    line: usize,
    previous: Token,
}

impl Lexer {
    /// Reads code until the end of the source or, when `nested` inside a
    /// template substitution, the `}` that closes it.
    fn code(&mut self, nested: bool) {
        let mut depth = 0_usize;
        while let Some(current) = self.peek(0) {
            match current {
                '/' if self.peek(1) == Some('/') => {
                    while self.peek(0).is_some_and(|char| char != '\n') {
                        self.advance();
                    }
                }
                '/' if self.peek(1) == Some('*') => {
                    self.advance();
                    self.advance();
                    while self.peek(0).is_some()
                        && !(self.peek(0) == Some('*') && self.peek(1) == Some('/'))
                    {
                        self.advance();
                    }
                    self.advance();
                    self.advance();
                }
                '\'' | '"' => {
                    let selector = self.previous == Token::Punct('(')
                        && matches!(&self.before_previous, Token::Word(word) if SELECTOR_METHODS.contains(&word.as_str()));
                    self.string(current, selector);
                    self.set(Token::Value);
                }
                '`' => {
                    self.advance();
                    self.template();
                    self.set(Token::Value);
                }
                '/' if self.starts_regex() => {
                    self.regex();
                    self.set(Token::Value);
                }
                '}' if nested && depth == 0 => {
                    self.advance();

                    return;
                }
                _ if current.is_alphanumeric() || current == '_' || current == '$' => {
                    let mut word = String::new();
                    while let Some(char) = self
                        .peek(0)
                        .filter(|char| char.is_alphanumeric() || *char == '_' || *char == '$')
                    {
                        word.push(char);
                        self.advance();
                    }
                    let token = if current.is_ascii_digit() {
                        Token::Value
                    } else {
                        Token::Word(word)
                    };
                    self.set(token);
                }
                _ if current.is_whitespace() => self.advance(),
                _ => {
                    depth = match current {
                        '{' => depth + 1,
                        '}' => depth.saturating_sub(1),
                        _ => depth,
                    };
                    self.advance();
                    self.set(Token::Punct(current));
                }
            }
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.index + offset).copied()
    }

    fn advance(&mut self) {
        if self.peek(0) == Some('\n') {
            self.line += 1;
        }
        self.index += 1;
    }

    fn set(&mut self, token: Token) {
        self.before_previous = mem::replace(&mut self.previous, token);
    }

    /// Returns whether a `/` here starts a regular expression: it does
    /// where an expression may begin, not after one.
    fn starts_regex(&self) -> bool {
        match &self.previous {
            Token::None => true,
            Token::Punct(char) => !matches!(char, ')' | ']' | '}'),
            Token::Value => false,
            Token::Word(word) => REGEX_KEYWORDS.contains(&word.as_str()),
        }
    }

    /// Reads a quoted string, which ends at its closing quote or, when
    /// unterminated, at the end of the line, and scans it unless it is a
    /// selector.
    fn string(&mut self, quote: char, selector: bool) {
        let line = self.line;
        let mut text = String::new();
        self.advance();
        while let Some(char) = self.peek(0).filter(|char| *char != '\n') {
            self.advance();
            if char == quote {
                break;
            }
            if char == '\\' {
                text.extend(self.peek(0));
                self.advance();
            } else {
                text.push(char);
            }
        }
        if !selector {
            self.scan(&text, line);
        }
    }

    /// Reads a template literal after its opening backtick, scanning each
    /// run of literal text and lexing each substitution as code.
    fn template(&mut self) {
        let mut line = self.line;
        let mut text = String::new();
        while let Some(char) = self.peek(0) {
            self.advance();
            match char {
                '`' => break,
                '\\' => {
                    text.extend(self.peek(0));
                    self.advance();
                }
                '$' if self.peek(0) == Some('{') => {
                    self.scan(&mem::take(&mut text), line);
                    self.advance();
                    self.previous = Token::None;
                    self.code(true);
                    line = self.line;
                }
                _ => text.push(char),
            }
        }
        self.scan(&text, line);
    }

    /// Reads a regular expression literal and its flags without scanning
    /// it.
    fn regex(&mut self) {
        let mut class = false;
        self.advance();
        while let Some(char) = self.peek(0).filter(|char| *char != '\n') {
            self.advance();
            match char {
                '\\' => self.advance(),
                '[' => class = true,
                ']' => class = false,
                '/' if !class => break,
                _ => {}
            }
        }
        while self.peek(0).is_some_and(char::is_alphanumeric) {
            self.advance();
        }
    }

    /// Reports the hex colours and colour functions with literal channels
    /// in `text`, a literal that starts on `line`.
    fn scan(&mut self, text: &str, line: usize) {
        let chars: Vec<char> = text.chars().collect();
        let is_word = |char: &char| char.is_ascii_alphanumeric() || *char == '_' || *char == '-';
        let mut line = line;
        let mut index = 0;
        while index < chars.len() {
            let start = index;
            let after_word = start > 0 && (is_word(&chars[start - 1]) || chars[start - 1] == '&');
            index += 1;
            if chars[start] == '\n' {
                line += 1;
            } else if chars[start] == '#' && !after_word {
                while chars.get(index).is_some_and(is_word) {
                    index += 1;
                }
                let digits = &chars[start + 1..index];
                if matches!(digits.len(), 3 | 4 | 6 | 8)
                    && digits.iter().all(char::is_ascii_hexdigit)
                {
                    self.found.push(RawColor {
                        line,
                        text: chars[start..index].iter().collect(),
                    });
                }
            } else if chars[start].is_ascii_alphabetic() && !after_word {
                while chars.get(index).is_some_and(is_word) {
                    index += 1;
                }
                let word = chars[start..index]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                let channel = chars[index..]
                    .iter()
                    .skip(1)
                    .find(|char| !char.is_whitespace())
                    .is_some_and(|char| {
                        char.is_ascii_digit()
                            || matches!(char, '.' | '-' | '+')
                            || (word == "color" && char.is_ascii_alphabetic())
                    });
                if chars.get(index) == Some(&'(')
                    && COLOR_FUNCTIONS.contains(&word.as_str())
                    && channel
                {
                    self.found.push(RawColor {
                        line,
                        text: format!("{word}()"),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "script_test.rs"]
mod tests;
