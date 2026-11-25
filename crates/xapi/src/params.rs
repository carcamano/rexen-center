//! Helpers the generated calls use to build positional parameters.

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// Serializes one parameter. The generated types always serialize.
pub fn arg<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Appends optional trailing parameters.
///
/// XAPI's newer optional parameters are positional and trailing, and older
/// servers reject calls that pass parameters they do not know. So parameters
/// are sent up to the last one that is `Some`; a `None` before a `Some`
/// cannot be expressed and is an error.
pub fn push_optional(
    params: &mut Vec<Value>,
    method: &str,
    optional: Vec<Option<Value>>,
) -> Result<()> {
    let Some(last) = optional.iter().rposition(Option::is_some) else {
        return Ok(());
    };
    for value in optional.into_iter().take(last + 1) {
        match value {
            Some(v) => params.push(v),
            None => {
                return Err(Error::Protocol(format!(
                    "{method}: an optional parameter was left out before one that is set"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn trailing_optionals() {
        let mut p = vec![json!(1)];
        push_optional(&mut p, "m", vec![None, None]).unwrap();
        assert_eq!(p, vec![json!(1)]);
        push_optional(&mut p, "m", vec![Some(json!(2)), None]).unwrap();
        assert_eq!(p, vec![json!(1), json!(2)]);
        assert!(push_optional(&mut p, "m", vec![None, Some(json!(3))]).is_err());
    }
}
