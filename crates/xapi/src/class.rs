//! Traits implemented by the generated class marker types.

use std::fmt::Debug;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A XenAPI class, identified by a zero-sized marker type such as
/// [`crate::Vm`]. Markers are used as the type parameter of [`crate::Ref`].
pub trait XapiClass: Sized + 'static {
    /// Class name as the API spells it (`"VM"`, `"host"`, `"VM_guest_metrics"`).
    /// Event classes are the same names in lower case.
    const NAME: &'static str;

    /// Whether `class` (as found in an event, any case) names this class.
    fn is_class(class: &str) -> bool {
        class.eq_ignore_ascii_case(Self::NAME)
    }
}

/// A XenAPI class that has a record type.
pub trait XapiObject: XapiClass {
    type Record: DeserializeOwned
        + Serialize
        + Clone
        + Default
        + Debug
        + PartialEq
        + Send
        + Sync
        + 'static;
}
