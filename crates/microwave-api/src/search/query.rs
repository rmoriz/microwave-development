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
//! - wildcard field names: `*:value`, `key*:value`, `k?y:value`
//! - `field:[a TO b]` and `field:{a TO b}` ranges
//! - `AND`/`&&`, `OR`/`||`, `NOT`, `!`, `+`, `-`, parentheses
//! - fuzzy terms (`value~`, `value~1`) and boosts (`value^2`)
//!
//! As in Lucene and the upstream Chef grammar, adjacent expressions without an
//! operator are OR-ed (`a b` means `a OR b`).

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
        /// `Some("")` for a bare `~` (Lucene's default distance), otherwise
        /// the numeric edit distance. `None` means an exact term.
        fuzzy: Option<String>,
        /// Text after `^`, normalized only by validating that it is numeric.
        boost: Option<String>,
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

/// Flattened document fields: field name to values. Field lookups are
/// case-insensitive.
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

    /// Renders the parsed query in the upstream Chef/Lucene form used by the
    /// OpenSearch and Elasticsearch backends.
    ///
    /// Chef indexes each document as a single whitespace-tokenized `content`
    /// field holding `key__=__value` pairs, so `field:value` becomes
    /// `content:field__=__value` (see `chef_lucene.peg` upstream). Rendering
    /// from the parsed AST, rather than forwarding the raw query string,
    /// keeps the two backends semantically aligned and avoids query injection.
    pub fn to_upstream_query(&self) -> String {
        render_upstream(self)
    }

    /// Evaluates the query against a flattened document.
    pub fn matches(&self, fields: &Fields) -> bool {
        match self {
            Query::MatchAll => true,
            Query::Term {
                field,
                pattern,
                fuzzy,
                ..
            } => match field.as_deref() {
                Some(name) => match_values(fields, name, |candidate| {
                    value_matches(pattern, candidate, false, fuzzy.as_deref())
                }),
                None => match_values(fields, "*", |candidate| {
                    value_matches(pattern, candidate, true, fuzzy.as_deref())
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

/// Field holding the flattened `key__=__value` document in Chef's OpenSearch
/// mapping (see `opensearch-index-init.json` upstream).
const CONTENT_FIELD: &str = "content";
const PAIR_SEPARATOR: &str = "__=__";
/// Upper bound used by the upstream grammar for open-ended ranges.
const RANGE_MAX: &str = "\\ufff0";

fn render_upstream(query: &Query) -> String {
    match query {
        Query::MatchAll => "*:*".to_string(),
        Query::Term {
            field,
            pattern,
            fuzzy,
            boost,
        } => {
            let mut rendered = match field {
                Some(field) if field == "*" => render_term_with_pair("*", pattern),
                Some(field) => render_term_with_pair(&escape_field(field), pattern),
                None => escape_value(pattern),
            };
            if let Some(fuzzy) = fuzzy {
                rendered.push('~');
                rendered.push_str(fuzzy);
            }
            if let Some(boost) = boost {
                rendered.push('^');
                rendered.push_str(boost);
            }
            rendered
        }
        Query::Exists { field } => {
            format!("{CONTENT_FIELD}:{}{PAIR_SEPARATOR}*", escape_field(field))
        }
        Query::Range {
            field,
            start,
            end,
            inclusive,
        } => {
            let field = escape_field(field);
            let (open, close) = if *inclusive { ("[", "]") } else { ("{", "}") };
            let start = start
                .as_deref()
                .map(escape_value)
                .unwrap_or_else(|| "*".to_string());
            let end = end
                .as_deref()
                .map(escape_value)
                .unwrap_or_else(|| RANGE_MAX.to_string());
            format!(
                "{CONTENT_FIELD}:{open}{field}{PAIR_SEPARATOR}{start} TO {field}{PAIR_SEPARATOR}{end}{close}"
            )
        }
        Query::And(left, right) => {
            format!("({} AND {})", render_upstream(left), render_upstream(right))
        }
        Query::Or(left, right) => {
            format!("({} OR {})", render_upstream(left), render_upstream(right))
        }
        Query::Not(query) => format!("NOT ({})", render_upstream(query)),
    }
}

/// Renders a `content:field__=__value` term. Multi-word values are rendered as
/// a phrase covering the whole pair, matching upstream's `field_phrase` rule:
/// `content:"field__=__value with spaces"`.
fn render_term_with_pair(field: &str, pattern: &str) -> String {
    if pattern.chars().any(char::is_whitespace) {
        format!(
            "{CONTENT_FIELD}:\"{field}{PAIR_SEPARATOR}{}\"",
            escape_with_wildcards(pattern, true)
        )
    } else {
        format!(
            "{CONTENT_FIELD}:{field}{PAIR_SEPARATOR}{}",
            escape_with_wildcards(pattern, true)
        )
    }
}

fn escape_field(field: &str) -> String {
    // Field names may contain `*`/`?` wildcards; they must reach the remote
    // query parser unescaped so `*:value` and `key*:value` keep their meaning.
    escape_with_wildcards(field, true)
}

/// Escapes a value while preserving `*`/`?` wildcards, matching Lucene's
/// `query_string` syntax.
fn escape_value(value: &str) -> String {
    let escaped = escape_with_wildcards(value, true);
    if escaped.chars().any(char::is_whitespace) {
        // Bare terms containing whitespace only make sense as phrases.
        format!("\"{}\"", escape_with_wildcards(value, false))
    } else {
        escaped
    }
}

fn escape_with_wildcards(value: &str, wildcards: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '+' | '-' | '=' | '&' | '|' | '>' | '<' | '!' | '(' | ')' | '{' | '}' | '['
            | ']' | '^' | '"' | '~' | ':' | '/' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '*' | '?' if !wildcards => {
                escaped.push('\\');
                escaped.push(character);
            }
            other => escaped.push(other),
        }
    }
    escaped
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

fn value_matches(pattern: &str, candidate: &str, bare: bool, fuzzy: Option<&str>) -> bool {
    if bare {
        // Upstream evaluates bare terms against the whitespace-tokenized
        // `content` field. Match whole tokens only, so `bar` does not match
        // the `foo-bar` token (Chef's `foo* AND NOT bar` case).
        candidate
            .split_whitespace()
            .any(|token| term_matches(pattern, token, fuzzy))
    } else {
        term_matches(pattern, candidate, fuzzy)
    }
}

fn term_matches(pattern: &str, candidate: &str, fuzzy: Option<&str>) -> bool {
    if let Some(distance) = fuzzy {
        // The embedded backend approximates Lucene fuzzy matching with
        // Levenshtein edit distance. Boosts do not affect matching.
        levenshtein_within(
            &pattern.to_lowercase(),
            &candidate.to_lowercase(),
            fuzzy_distance(distance),
        )
    } else if pattern.contains('*') || pattern.contains('?') {
        glob_match(pattern, candidate)
    } else {
        pattern.eq_ignore_ascii_case(candidate)
    }
}

/// Interprets a Lucene fuzzy parameter. A bare `~` uses the Lucene default of
/// two edits. Decimal values (the old Lucene similarity form) are rounded up so
/// `~0.5` behaves like one edit.
fn fuzzy_distance(value: &str) -> usize {
    let Ok(parsed) = value.parse::<f64>() else {
        return 2;
    };
    if !parsed.is_finite() || parsed <= 0.0 {
        return 0;
    }
    parsed.ceil().min(2.0) as usize
}

/// Case-insensitive Levenshtein distance check with an early exit.
fn levenshtein_within(left: &str, right: &str, max_distance: usize) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > max_distance {
        return false;
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(substitution);
        }
        if current.iter().min().copied().unwrap_or(0) > max_distance {
            return false;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()] <= max_distance
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
    /// `~` with an optional numeric parameter (`None` for a bare `~`).
    Fuzzy(Option<String>),
    /// `^` with its required numeric parameter.
    Boost(String),
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
            '&' => {
                if characters.get(index + 1) != Some(&'&') {
                    return Err(QueryError::new("expected '&&' in query"));
                }
                tokens.push(Token::Word("&&".to_string()));
                index += 2;
            }
            '|' => {
                if characters.get(index + 1) != Some(&'|') {
                    return Err(QueryError::new("expected '||' in query"));
                }
                tokens.push(Token::Word("||".to_string()));
                index += 2;
            }
            '~' => {
                let (parameter, next) = read_modifier_parameter(&characters, index + 1);
                tokens.push(Token::Fuzzy(parameter));
                index = next;
            }
            '^' => {
                let (parameter, next) = read_modifier_parameter(&characters, index + 1);
                let Some(parameter) = parameter else {
                    return Err(QueryError::new("boost requires a numeric value"));
                };
                tokens.push(Token::Boost(parameter));
                index = next;
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
            || matches!(
                character,
                '(' | ')' | '[' | ']' | '{' | '}' | ':' | '"' | '~' | '^' | '&' | '|'
            )
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

/// Reads the optional numeric parameter attached to `~` or `^`.
///
/// Only contiguous digits (and a single decimal point followed by digits) are
/// consumed, so `term~ other` does not treat `other` as the fuzzy parameter.
fn read_modifier_parameter(characters: &[char], mut index: usize) -> (Option<String>, usize) {
    let start = index;
    while index < characters.len() && characters[index].is_ascii_digit() {
        index += 1;
    }
    if index > start
        && index + 1 < characters.len()
        && characters[index] == '.'
        && characters[index + 1].is_ascii_digit()
    {
        index += 1;
        while index < characters.len() && characters[index].is_ascii_digit() {
            index += 1;
        }
    }
    if index == start {
        (None, index)
    } else {
        (Some(characters[start..index].iter().collect()), index)
    }
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
        loop {
            match self.peek() {
                Some(Token::Word(word)) if word == "OR" || word == "||" => {
                    self.position += 1;
                    let right = self.parse_and()?;
                    left = Query::Or(Box::new(left), Box::new(right));
                }
                // Required/prohibited clauses (`a -b`, `a +b`, `a !b`,
                // `a NOT b`) keep their historic conjunctive meaning rather
                // than taking on Lucene's default OR for plain adjacency.
                Some(token) if starts_unary_expression(token) => {
                    let right = self.parse_and()?;
                    left = Query::And(Box::new(left), Box::new(right));
                }
                // Lucene/Chef treat adjacent expressions as OR-ed. Explicit
                // `AND` still binds tighter and is handled by `parse_and`.
                Some(token) if starts_expression(token) => {
                    let right = self.parse_and()?;
                    left = Query::Or(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Query, QueryError> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::Word(word)) if word == "AND" || word == "&&" => {
                    self.position += 1;
                    let right = self.parse_unary()?;
                    left = Query::And(Box::new(left), Box::new(right));
                }
                _ => break,
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
                } else if is_operator(&name) {
                    Err(QueryError::new(format!("unexpected operator {name}")))
                } else if name == "*" {
                    Ok(Query::MatchAll)
                } else {
                    let (fuzzy, boost) = self.parse_term_modifiers()?;
                    Ok(Query::Term {
                        field: None,
                        pattern: name,
                        fuzzy,
                        boost,
                    })
                }
            }
            Some(Token::Phrase(phrase)) => {
                let (fuzzy, boost) = self.parse_term_modifiers()?;
                Ok(Query::Term {
                    field: None,
                    pattern: phrase,
                    fuzzy,
                    boost,
                })
            }
            Some(_) => Err(QueryError::new("expected a search term")),
            None => Err(QueryError::new("unexpected end of query")),
        }
    }

    fn parse_field_value(&mut self, field: String) -> Result<Query, QueryError> {
        match self.next() {
            Some(Token::Phrase(phrase)) => {
                let (fuzzy, boost) = self.parse_term_modifiers()?;
                Ok(Query::Term {
                    field: Some(field),
                    pattern: phrase,
                    fuzzy,
                    boost,
                })
            }
            Some(Token::Word(value)) => {
                if field == "*" && value == "*" {
                    Ok(Query::MatchAll)
                } else if value == "*" {
                    self.reject_term_modifiers()?;
                    Ok(Query::Exists { field })
                } else {
                    let (fuzzy, boost) = self.parse_term_modifiers()?;
                    Ok(Query::Term {
                        field: Some(field),
                        pattern: value,
                        fuzzy,
                        boost,
                    })
                }
            }
            Some(Token::LeftBracket) => self.parse_range(field, true, Token::RightBracket),
            Some(Token::LeftBrace) => self.parse_range(field, false, Token::RightBrace),
            Some(_) => Err(QueryError::new("expected a value after ':'")),
            None => Err(QueryError::new("expected a value after ':'")),
        }
    }

    fn parse_term_modifiers(&mut self) -> Result<(Option<String>, Option<String>), QueryError> {
        let mut fuzzy = None;
        let mut boost = None;
        loop {
            match self.peek() {
                Some(Token::Fuzzy(parameter)) => {
                    if fuzzy.is_some() {
                        return Err(QueryError::new("duplicate fuzzy modifier"));
                    }
                    fuzzy = Some(parameter.clone().unwrap_or_default());
                    self.position += 1;
                }
                Some(Token::Boost(parameter)) => {
                    if boost.is_some() {
                        return Err(QueryError::new("duplicate boost modifier"));
                    }
                    boost = Some(parameter.clone());
                    self.position += 1;
                }
                _ => break,
            }
        }
        Ok((fuzzy, boost))
    }

    fn reject_term_modifiers(&mut self) -> Result<(), QueryError> {
        let (fuzzy, boost) = self.parse_term_modifiers()?;
        if fuzzy.is_some() || boost.is_some() {
            return Err(QueryError::new("modifiers are not valid here"));
        }
        Ok(())
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

fn is_operator(word: &str) -> bool {
    matches!(word, "AND" | "OR" | "NOT" | "&&" | "||")
}

fn starts_unary_expression(token: &Token) -> bool {
    match token {
        Token::Bang | Token::Plus | Token::Minus => true,
        Token::Word(word) => word == "NOT",
        _ => false,
    }
}

fn starts_expression(token: &Token) -> bool {
    match token {
        Token::Word(word) => !is_operator(word),
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
            ("hello", true),
            ("apache", false),
        ];

        for (query, expected) in cases {
            let parsed = Query::parse(query).unwrap();
            assert_eq!(parsed.matches(&document), expected, "query {query}");
        }
    }

    #[test]
    fn bare_terms_match_whitespace_tokens_not_substrings() {
        let foo = fields(&[("id", "foo"), ("name", "data_bag_item_x_foo")]);
        let foo_bar = fields(&[("id", "foo-bar"), ("name", "data_bag_item_x_foo-bar")]);

        let query = Query::parse("id:foo* AND NOT bar").unwrap();
        assert!(query.matches(&foo));
        assert!(
            query.matches(&foo_bar),
            "bare `bar` must not match the foo-bar token"
        );

        let query = Query::parse("bar").unwrap();
        assert!(!query.matches(&foo_bar));
        assert!(Query::parse("foo-bar").unwrap().matches(&foo_bar));

        let query = Query::parse("world").unwrap();
        assert!(query.matches(&fields(&[("description", "hello world")])));
    }

    #[test]
    fn renders_upstream_content_queries() {
        let cases = [
            ("*:*", "*:*"),
            ("name:web1", "content:name__=__web1"),
            ("name:web*", "content:name__=__web*"),
            (
                "description:\"hello world\"",
                "content:\"description__=__hello world\"",
            ),
            ("platform:*", "content:platform__=__*"),
            ("*:ubuntu", "content:*__=__ubuntu"),
            ("na*:web1", "content:na*__=__web1"),
            ("k?y:value", "content:k?y__=__value"),
            ("name:web~", "content:name__=__web~"),
            ("name:web~1", "content:name__=__web~1"),
            ("name:web~0.5", "content:name__=__web~0.5"),
            ("name:web^2", "content:name__=__web^2"),
            ("name:web~1^2", "content:name__=__web~1^2"),
            ("\"hello world\"^10", "\"hello world\"^10"),
            ("cpu:[5 TO 9]", "content:[cpu__=__5 TO cpu__=__9]"),
            ("cpu:{8 TO 9}", "content:{cpu__=__8 TO cpu__=__9}"),
            (
                "release:[* TO 9]",
                "content:[release__=__* TO release__=__9]",
            ),
            (
                "release:[1 TO *]",
                "content:[release__=__1 TO release__=__\\ufff0]",
            ),
            ("apache", "apache"),
            (
                "name:web1 AND platform:ubuntu",
                "(content:name__=__web1 AND content:platform__=__ubuntu)",
            ),
            (
                "name:web1 OR name:web2",
                "(content:name__=__web1 OR content:name__=__web2)",
            ),
            ("NOT name:web1", "NOT (content:name__=__web1)"),
        ];

        for (input, expected) in cases {
            let query = Query::parse(input).unwrap();
            assert_eq!(
                query.to_upstream_query(),
                expected,
                "query {input} rendered incorrectly"
            );
        }
    }

    #[test]
    fn adjacent_terms_default_to_or_and_aliases_match_lucene() {
        let document = fields(&[("a", "1"), ("b", "2")]);

        assert!(Query::parse("a:1 b:2").unwrap().matches(&document));
        assert!(
            Query::parse("a:1 c:3").unwrap().matches(&document),
            "adjacent terms must be OR-ed like upstream"
        );
        assert!(!Query::parse("a:1 AND c:3").unwrap().matches(&document));
        assert!(Query::parse("a:1 && b:2").unwrap().matches(&document));
        assert!(Query::parse("a:1&&b:2").unwrap().matches(&document));
        assert!(!Query::parse("a:1 && c:3").unwrap().matches(&document));
        assert!(Query::parse("a:1 || c:3").unwrap().matches(&document));
        assert!(Query::parse("a:1||c:3").unwrap().matches(&document));
        assert!(!Query::parse("c:3 || d:4").unwrap().matches(&document));

        // Required/prohibited clauses still combine with AND.
        assert!(!Query::parse("a:1 -b:2").unwrap().matches(&document));
        assert!(Query::parse("a:1 -c:3").unwrap().matches(&document));
        assert!(Query::parse("a:1 +b:2").unwrap().matches(&document));
        assert!(!Query::parse("a:1 +c:3").unwrap().matches(&document));
        assert!(!Query::parse("a:1 !b:2").unwrap().matches(&document));
        assert!(!Query::parse("a:1 NOT b:2").unwrap().matches(&document));

        assert_eq!(
            Query::parse("a:1 && b:2").unwrap().to_upstream_query(),
            "(content:a__=__1 AND content:b__=__2)"
        );
        assert_eq!(
            Query::parse("a:1 || b:2").unwrap().to_upstream_query(),
            "(content:a__=__1 OR content:b__=__2)"
        );
        assert_eq!(
            Query::parse("a:1 b:2").unwrap().to_upstream_query(),
            "(content:a__=__1 OR content:b__=__2)"
        );
    }

    #[test]
    fn wildcard_field_names_and_phrases_are_supported() {
        let document = fields(&[
            ("name", "web1"),
            ("namespace", "web-team"),
            ("description", "hello big world"),
        ]);

        for query in ["na*:web1", "n?me:web1", "*:web1"] {
            assert!(
                Query::parse(query).unwrap().matches(&document),
                "query {query}"
            );
        }
        assert!(Query::parse("desc*:\"hello big world\"")
            .unwrap()
            .matches(&document));
        assert!(Query::parse("*:\"hello big world\"")
            .unwrap()
            .matches(&document));
        assert!(!Query::parse("name:web1")
            .unwrap()
            .matches(&fields(&[("name", "web1 extra"),])));
    }

    #[test]
    fn fuzzy_and_boost_modifiers_are_parsed_and_approximated() {
        let document = fields(&[("name", "web1"), ("description", "hello world")]);

        assert!(Query::parse("name:web1~").unwrap().matches(&document));
        assert!(Query::parse("name:wev1~1").unwrap().matches(&document));
        assert!(Query::parse("name:web1~1").unwrap().matches(&document));
        assert!(!Query::parse("name:wev1~0").unwrap().matches(&document));
        assert!(Query::parse("name:web1^2").unwrap().matches(&document));
        assert!(Query::parse("web1~1").unwrap().matches(&document));

        // Fuzzy matching applies to field values, not to the whitespace
        // tokens of a multi-word value as a whole.
        assert!(Query::parse("description:\"hello world\"~1")
            .unwrap()
            .matches(&document));
    }

    #[test]
    fn metadata_fields_are_queryable_like_content_tokens() {
        let document = fields(&[
            ("X_CHEF_id_CHEF_X", "node-guid"),
            ("X_CHEF_type_CHEF_X", "node"),
            ("X_CHEF_database_CHEF_X", "chef_acme"),
            ("data_bag", "secrets"),
        ]);

        for query in [
            "X_CHEF_id_CHEF_X:node-guid",
            "X_CHEF_type_CHEF_X:node",
            "X_CHEF_database_CHEF_X:chef_*",
            "data_bag:secrets",
            "*:node-guid",
        ] {
            assert!(
                Query::parse(query).unwrap().matches(&document),
                "query {query}"
            );
        }
    }

    #[test]
    fn rejects_malformed_queries() {
        for query in [
            "",
            "name:",
            "(name:web1",
            "name:[1 TO",
            "\"unterminated",
            "^2",
            "name:web^",
            "name:web~1~2",
            "name:web^1^2",
            "~1",
            "a:1 & b:2",
            "a:1 | b:2",
        ] {
            assert!(Query::parse(query).is_err(), "query {query:?}");
        }
    }
}
