use std::fmt;
use std::str::FromStr;

/// A Chef cookbook version.
///
/// Chef accepts both `MAJOR.MINOR` and `MAJOR.MINOR.PATCH`; missing components
/// are normalized to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const ZERO: Self = Self {
        major: 0,
        minor: 0,
        patch: 0,
    };

    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse a Chef version string.
    ///
    /// One to three dot-separated non-negative integers are accepted. Negative
    /// components and non-numeric input are rejected, matching the upstream
    /// database-backed validation.
    pub fn parse(input: &str) -> Result<Self, VersionError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(VersionError::new(input, "version is empty"));
        }

        let parts: Vec<&str> = trimmed.split('.').collect();
        if parts.len() > 3 {
            return Err(VersionError::new(input, "version has more than 3 parts"));
        }

        let mut numbers = [0_u64; 3];
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(VersionError::new(
                    input,
                    "version components must be non-negative integers",
                ));
            }
            numbers[index] = part
                .parse()
                .map_err(|_| VersionError::new(input, "version component is too large"))?;
        }

        Ok(Self::new(numbers[0], numbers[1], numbers[2]))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionError {
    input: String,
    message: &'static str,
}

impl VersionError {
    fn new(input: &str, message: &'static str) -> Self {
        Self {
            input: input.to_string(),
            message,
        }
    }
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid version '{}': {}",
            self.input, self.message
        )
    }
}

impl std::error::Error for VersionError {}

/// A cookbook version constraint operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equal,
    Greater,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    /// Pessimistic constraint (`~>`), using RubyGems-style semantics.
    Pessimistic,
}

impl Operator {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::Greater => ">",
            Self::Less => "<",
            Self::GreaterOrEqual => ">=",
            Self::LessOrEqual => "<=",
            Self::Pessimistic => "~>",
        }
    }
}

/// A parsed cookbook version constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub operator: Operator,
    pub version: Version,
    /// Number of components supplied for the version. This matters for `~>`:
    /// `~> 2.1` is `< 3.0.0`, while `~> 2.1.1` is `< 2.2.0`.
    pub precision: u8,
}

impl Constraint {
    /// Parse an upstream constraint string such as `>= 1.2.3`, `~> 2.1`, or a
    /// bare version (which implies `=`).
    pub fn parse(input: &str) -> Result<Self, ConstraintError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ConstraintError::new(input, "constraint is empty"));
        }

        let (operator, version_text) = if let Some(rest) = trimmed.strip_prefix(">=") {
            (Operator::GreaterOrEqual, rest)
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            (Operator::LessOrEqual, rest)
        } else if let Some(rest) = trimmed.strip_prefix("~>") {
            (Operator::Pessimistic, rest)
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            (Operator::Equal, rest)
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            (Operator::Greater, rest)
        } else if let Some(rest) = trimmed.strip_prefix('<') {
            (Operator::Less, rest)
        } else {
            (Operator::Equal, trimmed)
        };

        let version_text = version_text.trim();
        let version = Version::parse(version_text)
            .map_err(|_| ConstraintError::new(input, "invalid version"))?;
        let precision = version_text.split('.').count();
        let precision =
            u8::try_from(precision).map_err(|_| ConstraintError::new(input, "invalid version"))?;

        Ok(Self {
            operator,
            version,
            precision,
        })
    }

    /// A constraint matching exactly one version.
    pub fn exact(version: Version) -> Self {
        Self {
            operator: Operator::Equal,
            version,
            precision: 3,
        }
    }

    /// The implicit run-list constraint used for unversioned cookbooks.
    pub fn any() -> Self {
        Self::parse(">= 0.0.0").expect("static constraint is valid")
    }

    pub fn matches(&self, candidate: &Version) -> bool {
        match self.operator {
            Operator::Equal => candidate == &self.version,
            Operator::Greater => candidate > &self.version,
            Operator::Less => candidate < &self.version,
            Operator::GreaterOrEqual => candidate >= &self.version,
            Operator::LessOrEqual => candidate <= &self.version,
            Operator::Pessimistic => {
                candidate >= &self.version
                    && self
                        .pessimistic_upper_bound()
                        .is_none_or(|upper| candidate < &upper)
            }
        }
    }

    /// RubyGems-style upper bound: drop the last supplied component, then
    /// increment the component before it.
    fn pessimistic_upper_bound(&self) -> Option<Version> {
        let (major, minor) = match self.precision {
            0..=2 => (self.version.major.checked_add(1)?, 0),
            3 => (self.version.major, self.version.minor.checked_add(1)?),
            _ => return None,
        };
        Some(Version::new(major, minor, 0))
    }

    /// Format this constraint as `(name OP version)`, as used in upstream error
    /// documents.
    pub fn format_for(&self, name: &str) -> String {
        format!("({name} {} {})", self.operator.symbol(), self.version)
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.operator.symbol(), self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintError {
    input: String,
    message: &'static str,
}

impl ConstraintError {
    fn new(input: &str, message: &'static str) -> Self {
        Self {
            input: input.to_string(),
            message,
        }
    }
}

impl fmt::Display for ConstraintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid constraint '{}': {}",
            self.input, self.message
        )
    }
}

impl std::error::Error for ConstraintError {}
