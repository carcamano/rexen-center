//! Typed opaque references (`OpaqueRef:…`).

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The wire value XAPI uses for "no object".
pub const NULL_REF: &str = "OpaqueRef:NULL";

/// A reference to a XAPI object of class `T`.
///
/// `T` is a marker type (for example [`crate::Vm`]); it only exists at compile
/// time so a VM reference cannot be passed where a host reference is expected.
pub struct Ref<T> {
    opaque: String,
    _class: PhantomData<fn() -> T>,
}

impl<T> Ref<T> {
    /// Wraps a raw `OpaqueRef:…` string.
    pub fn new(opaque: impl Into<String>) -> Self {
        Self {
            opaque: opaque.into(),
            _class: PhantomData,
        }
    }

    /// The null reference.
    pub fn null() -> Self {
        Self::new(NULL_REF)
    }

    /// Whether this is the null reference (or empty).
    pub fn is_null(&self) -> bool {
        self.opaque.is_empty() || self.opaque == NULL_REF
    }

    /// `None` for the null reference, `Some(self)` otherwise.
    pub fn non_null(&self) -> Option<&Self> {
        (!self.is_null()).then_some(self)
    }

    /// The raw wire string.
    pub fn as_str(&self) -> &str {
        &self.opaque
    }
}

impl<T> Default for Ref<T> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Self::new(self.opaque.clone())
    }
}

impl<T> PartialEq for Ref<T> {
    fn eq(&self, other: &Self) -> bool {
        self.opaque == other.opaque
    }
}

impl<T> Eq for Ref<T> {}

impl<T> PartialOrd for Ref<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Ref<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.opaque.cmp(&other.opaque)
    }
}

impl<T> Hash for Ref<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.opaque.hash(state);
    }
}

impl<T> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ref({})", self.opaque)
    }
}

impl<T> fmt::Display for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.opaque)
    }
}

impl<T> Serialize for Ref<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.opaque)
    }
}

impl<'de, T> Deserialize<'de> for Ref<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Marker;

    #[test]
    fn null_detection_and_serde_roundtrip() {
        let r: Ref<Marker> = serde_json::from_str("\"OpaqueRef:NULL\"").unwrap();
        assert!(r.is_null());
        assert!(r.non_null().is_none());
        let r: Ref<Marker> = serde_json::from_str("\"OpaqueRef:abc\"").unwrap();
        assert!(!r.is_null());
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"OpaqueRef:abc\"");
        assert_eq!(r, Ref::new("OpaqueRef:abc"));
    }
}
