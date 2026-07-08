use crate::error::{GrsError, Result};
use std::fmt;

/// A session id newtype over a [`ulid::Ulid`].
///
/// ULIDs are 26-char, lexicographically-sortable by time, so `sessions/` dirs
/// sort chronologically for free (see `plan/02-storage-format.md`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub ulid::Ulid);

impl SessionId {
    /// Generate a new time-based (monotonic) id.
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }

    /// The canonical 26-char string form.
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    /// Parse a 26-char ULID string.
    pub fn parse(s: &str) -> Result<Self> {
        ulid::Ulid::from_string(s)
            .map(Self)
            .map_err(|e| GrsError::NotFound(format!("invalid session id \"{s}\": {e}")))
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId({})", self.0)
    }
}

impl serde::Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        ulid::Ulid::from_string(&s)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = SessionId::new();
        let s = id.as_str();
        assert_eq!(s.len(), 26);
        let back = SessionId::parse(&s).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn sorts_chronologically() {
        // ULIDs generated later sort after earlier ones (within the same ms the
        // crate guarantees monotonic random-tail increment).
        let a = SessionId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = SessionId::new();
        assert!(a < b, "{a} should sort before {b}");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(SessionId::parse("not-a-ulid").is_err());
    }
}
