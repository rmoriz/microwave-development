//! A deliberately small Chef/Lucene query subset.
//!
//! The upstream server parses search queries with a PEG grammar
//! (`chef_lucene.peg`) and transforms them into Solr/OpenSearch queries. This
//! module implements the subset needed by the first search slice directly:
//! a tokenizer followed by a recursive-descent parser and an in-memory
//! evaluator over a flattened document.
//!
//! Supported syntax:
//!
//! - `*:*`
//! - `field:value`, `field:"value with spaces"`
//! - `field:*` (field exists), `field:value*`, `field:*value?`
//! - `field:[a TO b]` and `field:{a TO b}` ranges
//! - `AND`, `OR`, `NOT`, `!`, `+`, `-`, parentheses and implicit `AND`

use std::collections::BTreeMap;
use std::fmt;

/// A parsed search query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Every document matches (`*:*`).
    MatchAll,
    /// A value match. `field` is `None` for a bare term and `Some("*")` for
    /// the `*:value` wildcard-field form.
    Term {
        field: Option<String>,
        pattern: String,
    },
    /// A field existence check (`field:*`).
    Exists {
        field: String,
    },
    /// An inclusive (`[a TO b]`) or exclusive (`{a TO b}`) range.
    Range {
        field: String,
        start: Option<String>,
        end: Option<String>,
        inclusive: bool,
    },
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
    Not(Box<Query>),
}

/// Flattened document fields: lower-cased field name to values.
pub type Fields = BTreeMap<String, Vec<String>>;

impl Query {
    /// Parses a Chef search query string.
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let tokens = tokenize(input)?;
        if tokens.is_empty() {
            return Err(QueryError::new("query must not be empty"));
        }
        let mut parser = Parser {
            tokens,
            position: 0,
        };
        let query = parser.parse_or()?;
        if parser.position != parser.tokens.len() {
            return Err(QueryError::new("unexpected token after query"));
        }
        Ok(query)
    }

    /// Evaluates the query against a flattened document.
    pub fn matches(&self, fields: &Fields) -> bool {
        match self {
            Query::MatchAll => true,
            Query::Term { field, pattern } => match field.as_deref() {
                Some(name) => match_values(fields, name, |candidate| {
                    value_matches(pattern, candidate, false)
                }),
                None => match_values(fields, "*", |candidate| {
                    value_matches(pattern, candidate, true)
                }),
            },
            Query::Exists { field } => fields
                .iter()
                .any(|(name, values)| field_name_matches(field, name) && !values.is_empty()),
            Query::Range {
                field,
                start,
                end,
                inclusive,
            } => match_values(fields, field, |candidate| {
                range_matches(start.as_deref(), end.as_deref(), *inclusive, candidate)
            }),
            Query::And(left, right) => left.matches(fields) && right.matches(fields),
            Query::Or(left, right) => left.matches(fields) || right.matches(fields),
            Query::Not(query) => !query.matches(fields),
        }
    }
}

fn match_values(fields: &Fields, field_pattern: &str, predicate: impl Fn(&str) -> bool) -> bool {
    fields.iter().any(|(name, values)| {
        field_name_matches(field_pattern, name) && values.iter().any(|value| predicate(value))
    })
}

fn field_name_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        glob_match(pattern, name)
    } else {
        pattern.eq_ignore_ascii_case(name)
    }
}

fn value_matches(pattern: &str, candidate: &str, bare: bool) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        glob_match(pattern, candidate)
    } else if bare {
        candidate.to_lowercase().contains(&pattern.to_lowercase())
    } else {
        pattern.eq_ignore_ascii_case(candidate)
    }
}

fn range_matches(start: Option<&str>, end: Option<&str>, inclusive: bool, candidate: &str) -> bool {
    let numeric = start.and_then(parse_number);
    let numeric_end = end.and_then(parse_number);
    if let Some(candidate) = parse_number(candidate) {
        let lower_ok = match (start, numeric) {
            (None, _) => true,
            (Some(_), Some(start)) => {
                if inclusive {
                    candidate >= start
                } else {
                    candidate > start
                }
            }
            (Some(_), None) => return false,
        };
        let upper_ok = match (end, numeric_end) {
            (None, _) => true,
            (Some(_), Some(end)) => {
                if inclusive {
                    candidate <= end
                } else {
                    candidate < end
                }
            }
            (Some(_), None) => return false,
        };
        return lower_ok && upper_ok;
    }

    let candidate = candidate.to_lowercase();
    let lower_ok = match start {
        None => true,
        Some(start) => {
            let start = start.to_lowercase();
            if inclusive {
                candidate >= start
            } else {
                candidate > start
            }
        }
    };
    let upper_ok = match end {
        None => true,
        Some(end) => {
            let end = end.to_lowercase();
            if inclusive {
                candidate <= end
            } else {
                candidate < end
            }
        }
    };
    lower_ok && upper_ok
}

fn parse_number(value: &str) -> Option<f64> {
    value.parse::<f64>().ok()
}

/// Case-insensitive `*`/`?` glob matching.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut star: Option<usize> = None;
    let mut star_text_index = 0;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            star_text_index = text_index;
            pattern_index += 1;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_text_index += 1;
            text_index = star_text_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// A query syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    message: String,
}

impl QueryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Phrase(String),
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    Colon,
    Plus,
    Minus,
    Bang,
}

impl Token {
    fn word(&self) -> Option<&str> {
        match self {
            Token::Word(word) => Some(word),
            _ => None,
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryError> {
    let characters: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }

        match character {
            '(' => {
                tokens.push(Token::LeftParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RightParen);
                index += 1;
            }
            '[' => {
                tokens.push(Token::LeftBracket);
                index += 1;
            }
            ']' => {
                tokens.push(Token::RightBracket);
                index += 1;
            }
            '{' => {
                tokens.push(Token::LeftBrace);
                index += 1;
            }
            '}' => {
                tokens.push(Token::RightBrace);
                index += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                index += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                index += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                index += 1;
            }
            '!' => {
                tokens.push(Token::Bang);
                index += 1;
            }
            '"' => {
                let (phrase, next) = read_phrase(&characters, index + 1)?;
                tokens.push(Token::Phrase(phrase));
                index = next;
            }
            '\\' => {
                let (word, next) = read_word(&characters, index)?;
                tokens.push(Token::Word(word));
                index = next;
            }
            _ => {
                let (word, next) = read_word(&characters, index)?;
                if word.is_empty() {
                    return Err(QueryError::new("unexpected character in query"));
                }
                tokens.push(Token::Word(word));
                index = next;
            }
        }
    }

    Ok(tokens)
}

fn read_word(characters: &[char], mut index: usize) -> Result<(String, usize), QueryError> {
    let mut word = String::new();
    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace()
            || matches!(character, '(' | ')' | '[' | ']' | '{' | '}' | ':' | '"')
        {
            break;
        }
        if character == '\\' {
            index += 1;
            if index >= characters.len() {
                return Err(QueryError::new("trailing escape in query"));
            }
            word.push(characters[index]);
            index += 1;
            continue;
        }
        word.push(character);
        index += 1;
    }
    Ok((word, index))
}

fn read_phrase(characters: &[char], mut index: usize) -> Result<(String, usize), QueryError> {
    let mut phrase = String::new();
    while index < characters.len() {
        match characters[index] {
            '\\' => {
                index += 1;
                if index >= characters.len() {
                    return Err(QueryError::new("trailing escape in query"));
                }
                phrase.push(characters[index]);
                index += 1;
            }
            '"' => return Ok((phrase, index + 1)),
            character => {
                phrase.push(character);
                index += 1;
            }
        }
    }
    Err(QueryError::new("unterminated quoted string in query"))
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        if token.is_some() {
            self.position += 1;
        }
        token
    }

    fn parse_or(&mut self) -> Result<Query, QueryError> {
        let mut left = self.parse_and()?;
        while self.peek_word_is("OR") {
            self.position += 1;
            let right = self.parse_and()?;
            left = Query::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Query, QueryError> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                None | Some(Token::RightParen) => break,
                Some(Token::Word(word)) if word == "OR" => break,
                Some(Token::Word(word)) if word == "AND" => {
                    self.position += 1;
                    let right = self.parse_unary()?;
                    left = Query::And(Box::new(left), Box::new(right));
                }
                Some(token) if starts_expression(token) => {
                    let right = self.parse_unary()?;
                    left = Query::And(Box::new(left), Box::new(right));
                }
                Some(_) => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Query, QueryError> {
        match self.peek() {
            Some(Token::Bang) => {
                self.position += 1;
                Ok(Query::Not(Box::new(self.parse_unary()?)))
            }
            Some(Token::Word(word)) if word == "NOT" => {
                self.position += 1;
                Ok(Query::Not(Box::new(self.parse_unary()?)))
            }
            Some(Token::Plus) => {
                self.position += 1;
                self.parse_unary()
            }
            Some(Token::Minus) => {
                self.position += 1;
                Ok(Query::Not(Box::new(self.parse_unary()?)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Query, QueryError> {
        match self.next() {
            Some(Token::LeftParen) => {
                let query = self.parse_or()?;
                if !matches!(self.next(), Some(Token::RightParen)) {
                    return Err(QueryError::new("missing closing parenthesis"));
                }
                Ok(query)
            }
            Some(Token::Word(name)) => {
                if matches!(self.peek(), Some(Token::Colon)) {
                    self.position += 1;
                    self.parse_field_value(name)
                } else if name == "AND" || name == "OR" || name == "NOT" {
                    Err(QueryError::new(format!("unexpected operator {name}")))
                } else if name == "*" {
                    Ok(Query::MatchAll)
                } else {
                    Ok(Query::Term {
                        field: None,
                        pattern: name,
                    })
                }
            }
            Some(Token::Phrase(phrase)) => Ok(Query::Term {
                field: None,
                pattern: phrase,
            }),
            Some(_) => Err(QueryError::new("expected a search term")),
            None => Err(QueryError::new("unexpected end of query")),
        }
    }

    fn parse_field_value(&mut self, field: String) -> Result<Query, QueryError> {
        match self.next() {
            Some(Token::Phrase(phrase)) => Ok(Query::Term {
                field: Some(field),
                pattern: phrase,
            }),
            Some(Token::Word(value)) => {
                if field == "*" && value == "*" {
                    Ok(Query::MatchAll)
                } else if value == "*" {
                    Ok(Query::Exists { field })
                } else {
                    Ok(Query::Term {
                        field: Some(field),
                        pattern: value,
                    })
                }
            }
            Some(Token::LeftBracket) => self.parse_range(field, true, Token::RightBracket),
            Some(Token::LeftBrace) => self.parse_range(field, false, Token::RightBrace),
            Some(_) => Err(QueryError::new("expected a value after ':'")),
            None => Err(QueryError::new("expected a value after ':'")),
        }
    }

    fn parse_range(
        &mut self,
        field: String,
        inclusive: bool,
        closing: Token,
    ) -> Result<Query, QueryError> {
        let start = self.parse_range_bound()?;
        if !self.peek_word_is("TO") {
            return Err(QueryError::new("range is missing TO"));
        }
        self.position += 1;
        let end = self.parse_range_bound()?;
        if !matches!(self.next(), Some(token) if token == closing) {
            return Err(QueryError::new("range is missing its closing bracket"));
        }

        if start.is_none() && end.is_none() {
            return Ok(Query::Exists { field });
        }
        Ok(Query::Range {
            field,
            start,
            end,
            inclusive,
        })
    }

    fn parse_range_bound(&mut self) -> Result<Option<String>, QueryError> {
        match self.next() {
            Some(Token::Word(value)) if value == "*" => Ok(None),
            Some(Token::Word(value)) => Ok(Some(value)),
            Some(Token::Phrase(value)) => Ok(Some(value)),
            Some(_) => Err(QueryError::new("invalid range bound")),
            None => Err(QueryError::new("unterminated range")),
        }
    }

    fn peek_word_is(&self, expected: &str) -> bool {
        self.peek().and_then(Token::word) == Some(expected)
    }
}

fn starts_expression(token: &Token) -> bool {
    match token {
        Token::Word(word) => word != "AND" && word != "OR",
        Token::Phrase(_) | Token::LeftParen | Token::Bang | Token::Plus | Token::Minus => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Fields {
        let mut fields = Fields::new();
        for (name, value) in pairs {
            fields
                .entry((*name).to_string())
                .or_default()
                .push((*value).to_string());
        }
        fields
    }

    #[test]
    fn parses_and_matches_the_supported_forms() {
        let document = fields(&[
            ("name", "web1"),
            ("platform", "ubuntu"),
            ("description", "hello world"),
            ("cpu", "8"),
            ("run_list", "recipe[apache]"),
        ]);

        let cases = [
            ("*:*", true),
            ("name:web1", true),
            ("name:web2", false),
            ("name:web*", true),
            ("description:\"hello world\"", true),
            ("platform:*", true),
            ("missing:*", false),
            ("cpu:[5 TO 9]", true),
            ("cpu:{8 TO 9}", false),
            ("name:web1 AND platform:ubuntu", true),
            ("name:web1 OR name:web2", true),
            ("name:web1 AND NOT name:web1", false),
            ("!name:web1", false),
            ("(name:web1 OR name:web2) AND platform:ubuntu", true),
            ("+name:web1", true),
            ("-name:web1", false),
            ("*:ubuntu", true),
            ("apache", true),
        ];

        for (query, expected) in cases {
            let parsed = Query::parse(query).unwrap();
            assert_eq!(parsed.matches(&document), expected, "query {query}");
        }
    }

    #[test]
    fn rejects_malformed_queries() {
        for query in ["", "name:", "(name:web1", "name:[1 TO", "\"unterminated"] {
            assert!(Query::parse(query).is_err(), "query {query:?}");
        }
    }
}
