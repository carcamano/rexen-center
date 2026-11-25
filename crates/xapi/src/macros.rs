/// Declares a XenAPI string enum that tolerates values unknown to this build.
///
/// XAPI enums travel as strings; newer servers may add values, so every enum
/// gets an `Unknown(String)` variant instead of failing to deserialize. The
/// code generator (`crates/xapi-codegen`) emits every enum through this macro.
/// The default value is `Unknown("")` (a missing field).
#[macro_export]
macro_rules! xapi_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value not known to this build of the client.
            Unknown(String),
        }

        impl $name {
            /// Every value known to this build, in datamodel order.
            pub const VALUES: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// Whether this is a value unknown to this build (or missing).
            pub fn is_unknown(&self) -> bool {
                matches!(self, Self::Unknown(_))
            }

            /// The wire (JSON) representation of this value.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Unknown(s) => s.as_str(),
                }
            }
        }

        impl ::std::default::Default for $name {
            fn default() -> Self {
                Self::Unknown(::std::string::String::new())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::std::convert::Infallible;
            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                // XAPI enum values are case-insensitive on the wire.
                $( if s.eq_ignore_ascii_case($wire) { return Ok(Self::$variant); } )+
                Ok(Self::Unknown(s.to_owned()))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> ::std::result::Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> ::std::result::Result<Self, D::Error> {
                let s = <::std::string::String as ::serde::Deserialize>::deserialize(d)?;
                Ok(s.parse().unwrap_or_else(|never| match never {}))
            }
        }
    };
}
