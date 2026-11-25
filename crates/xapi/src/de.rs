//! Lenient deserializers used by the generated record structs.
//!
//! A record must decode even when one field is not what the datamodel says
//! (a newer or older server, an integer sent as a string, a `null`), so
//! scalars accept a few spellings and anything else falls back to its default.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// An integer sent as a JSON number or a decimal string.
pub fn int<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or_default(),
        Value::String(s) => s.trim().parse().unwrap_or_default(),
        _ => 0,
    })
}

/// A float sent as a JSON number or a string (including `NaN`/`inf`).
pub fn float<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64().unwrap_or_default(),
        Value::String(s) => s.trim().parse().unwrap_or_default(),
        _ => 0.0,
    })
}

/// A boolean sent as a JSON bool or the strings `true`/`false`.
pub fn bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => s.trim().eq_ignore_ascii_case("true"),
        Value::Number(n) => n.as_i64().is_some_and(|n| n != 0),
        _ => false,
    })
}

/// A string; numbers and booleans are stringified, `null` is empty.
pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// Any other type; a value that does not decode becomes the default.
pub fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(d)?;
    if value.is_null() {
        return Ok(T::default());
    }
    Ok(T::deserialize(value).unwrap_or_else(|e| {
        tracing::debug!(error = %e, "field did not decode; using its default");
        T::default()
    }))
}

/// A XenAPI `option`: `null`, `[]` or a missing field is `None`; `[x]` or a
/// bare `x` is `Some(x)`.
pub fn option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Ok(match Value::deserialize(d)? {
        Value::Null => None,
        Value::Array(mut items) if items.len() <= 1 => {
            items.pop().and_then(|v| T::deserialize(v).ok())
        }
        other => T::deserialize(other).ok(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[derive(serde::Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct Probe {
        #[serde(deserialize_with = "super::int")]
        i: i64,
        #[serde(deserialize_with = "super::float")]
        f: f64,
        #[serde(deserialize_with = "super::bool")]
        b: bool,
        #[serde(deserialize_with = "super::string")]
        s: String,
        #[serde(deserialize_with = "super::or_default")]
        v: Vec<i64>,
    }

    #[test]
    fn tolerates_odd_spellings() {
        let p: Probe =
            serde_json::from_value(json!({"i": "42", "f": "1.5", "b": "true", "s": 7, "v": "x"}))
                .unwrap();
        assert_eq!(
            p,
            Probe {
                i: 42,
                f: 1.5,
                b: true,
                s: "7".into(),
                v: vec![]
            }
        );
        let p: Probe = serde_json::from_value(json!({"i": null, "v": null})).unwrap();
        assert_eq!(p, Probe::default());
        let p: Probe = serde_json::from_value(json!({"v": [1, 2]})).unwrap();
        assert_eq!(p.v, vec![1, 2]);
    }
}
