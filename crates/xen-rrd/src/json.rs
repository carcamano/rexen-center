//! The JSON forms (`json=true`). XAPI writes every number as a string;
//! plain JSON numbers are accepted too.

use serde_json::Value;

use crate::{Cf, Result, Rra, Rrd, RrdError, SourceId, UpdateRow, Updates, number};

fn decode(msg: impl Into<String>) -> RrdError {
    RrdError::Decode(msg.into())
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::String(s) => number(s),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn int(v: Option<&Value>, what: &str) -> Result<i64> {
    v.and_then(num)
        .filter(|n| n.is_finite())
        .map(|n| n as i64)
        .ok_or_else(|| decode(format!("missing or bad `{what}`")))
}

pub(crate) fn updates(body: &[u8]) -> Result<Updates> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| decode(e.to_string()))?;
    let meta = doc.get("meta").ok_or_else(|| decode("no `meta`"))?;
    let legend = meta
        .get("legend")
        .and_then(Value::as_array)
        .ok_or_else(|| decode("no legend"))?
        .iter()
        .map(|e| {
            e.as_str()
                .and_then(SourceId::parse)
                .ok_or_else(|| decode(format!("bad legend entry {e}")))
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = doc
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            let t = int(row.get("t"), "t")?;
            let values = row
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| decode("row without values"))?
                .iter()
                .map(|v| num(v).unwrap_or(f64::NAN))
                .collect();
            Ok(UpdateRow { t, values })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Updates {
        start: int(meta.get("start"), "start")?,
        end: int(meta.get("end"), "end")?,
        step: int(meta.get("step"), "step")?,
        legend,
        rows,
    })
}

pub(crate) fn rrd(body: &[u8]) -> Result<Rrd> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| decode(e.to_string()))?;
    let sources = doc
        .get("ds")
        .and_then(Value::as_array)
        .ok_or_else(|| decode("no data sources"))?
        .iter()
        .map(|d| {
            d.get("name")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_owned())
                .ok_or_else(|| decode("data source without a name"))
        })
        .collect::<Result<Vec<_>>>()?;
    let archives = doc
        .get("rra")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|rra| {
            let cf = Cf::parse(rra.get("cf")?.as_str()?)?;
            let pdp_per_row = int(rra.get("pdp_per_row"), "pdp_per_row").ok()?;
            let rows = rra
                .get("database")?
                .as_array()?
                .iter()
                .map(|row| {
                    row.as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .map(|v| num(v).unwrap_or(f64::NAN))
                        .collect()
                })
                .collect();
            Some(Rra {
                cf,
                pdp_per_row,
                rows,
            })
        })
        .collect();
    Ok(Rrd {
        step: int(doc.get("step"), "step")?,
        last_update: doc
            .get("lastupdate")
            .and_then(num)
            .filter(|n| n.is_finite())
            .ok_or_else(|| decode("missing or bad `lastupdate`"))?,
        sources,
        archives,
    })
}
