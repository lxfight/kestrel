//! Zero-dependency SemVer 2.0.0 parser and comparator for release version checks.

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreId {
    Numeric(u64),
    AlphaNum(String),
}

impl Ord for PreId {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreId::Numeric(a), PreId::Numeric(b)) => a.cmp(b),
            // SemVer 2.0.0 §11.4.3: numeric identifiers always have lower precedence than non-numeric.
            (PreId::Numeric(_), PreId::AlphaNum(_)) => Ordering::Less,
            (PreId::AlphaNum(_), PreId::Numeric(_)) => Ordering::Greater,
            (PreId::AlphaNum(a), PreId::AlphaNum(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for PreId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreId::Numeric(n) => write!(f, "{n}"),
            PreId::AlphaNum(s) => f.write_str(s),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<PreId>,
}

impl Version {
    /// Parses a version string such as `0.2.0`, `v0.2.0-beta.1`, or `1.4.2+build.9`.
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let without_v = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);
        let without_build = without_v
            .split_once('+')
            .map_or(without_v, |(before, _)| before);
        let (core, pre_str) = match without_build.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (without_build, None),
        };

        let mut parts = core.split('.');
        let major = parse_numeric_part(parts.next()?)?;
        let minor = parse_numeric_part(parts.next()?)?;
        let patch = parse_numeric_part(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }

        let mut prerelease = Vec::new();
        if let Some(pre) = pre_str {
            if pre.is_empty() {
                return None;
            }
            for seg in pre.split('.') {
                if seg.is_empty()
                    || !seg
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return None;
                }
                if seg.bytes().all(|b| b.is_ascii_digit()) {
                    prerelease.push(PreId::Numeric(seg.parse().ok()?));
                } else {
                    prerelease.push(PreId::AlphaNum(seg.to_string()));
                }
            }
        }

        Some(Self {
            major,
            minor,
            patch,
            prerelease,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }
}

fn parse_numeric_part(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                (true, true) => Ordering::Equal,
                // Normal version has higher precedence than a pre-release version.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.prerelease.iter().zip(other.prerelease.iter()) {
                        let ord = a.cmp(b);
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    self.prerelease.len().cmp(&other.prerelease.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prerelease.is_empty() {
            f.write_str("-")?;
            for (idx, id) in self.prerelease.iter().enumerate() {
                if idx > 0 {
                    f.write_str(".")?;
                }
                write!(f, "{id}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Version;

    #[test]
    fn parses_standard_and_prefixed_versions() {
        let v = Version::parse("v0.2.1+20261127").unwrap();
        assert_eq!(v.to_string(), "0.2.1");
        assert!(!v.is_prerelease());

        let pre = Version::parse("V1.0.0-beta.12").unwrap();
        assert_eq!(pre.to_string(), "1.0.0-beta.12");
        assert!(pre.is_prerelease());

        assert!(Version::parse("invalid").is_none());
        assert!(Version::parse("1.2").is_none());
        assert!(Version::parse("1.2.3-").is_none());
        assert!(Version::parse("1.2.3-beta..1").is_none());
    }

    #[test]
    fn semver_ordering_matches_spec() {
        let chain = [
            "0.1.0",
            "0.2.0-alpha",
            "0.2.0-alpha.1",
            "0.2.0-alpha.beta",
            "0.2.0-beta",
            "0.2.0-beta.2",
            "0.2.0-beta.11",
            "0.2.0-rc.1",
            "0.2.0",
            "0.10.0",
            "1.0.0",
        ];
        let parsed: Vec<Version> = chain.iter().map(|s| Version::parse(s).unwrap()).collect();
        for pair in parsed.windows(2) {
            assert!(pair[0] < pair[1], "expected {} < {}", pair[0], pair[1]);
        }
        assert_eq!(
            Version::parse("0.2.0+build.1").unwrap(),
            Version::parse("v0.2.0+build.2").unwrap()
        );
    }
}
