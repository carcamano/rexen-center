//! The XML forms (the default, and the only one of servers without JSON
//! support): `<xport>` for `rrd_updates`, rrdtool's `<rrd>` dump for
//! `host_rrds` / `vm_rrds`. Read as a stream; elements this crate does not
//! use (`cdp_prep`, `params`…) are skipped.

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::{Cf, Result, Rra, Rrd, RrdError, SourceId, UpdateRow, Updates, number};

fn decode(msg: impl Into<String>) -> RrdError {
    RrdError::Decode(msg.into())
}

/// Calls `visit(path, None)` when an element opens and `visit(path,
/// Some(text))` for each text node, with `path` the names of the open
/// elements.
fn walk(body: &[u8], mut visit: impl FnMut(&[String], Option<&str>) -> Result<()>) -> Result<()> {
    let mut reader = Reader::from_reader(body);
    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                visit(&path, None)?;
            }
            Ok(Event::Empty(e)) => {
                path.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                visit(&path, None)?;
                path.pop();
            }
            Ok(Event::End(_)) => {
                path.pop();
            }
            Ok(Event::Text(t)) => {
                let text = t.decode().map_err(|e| decode(e.to_string()))?;
                if !text.trim().is_empty() {
                    visit(&path, Some(&text))?;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(decode(e.to_string())),
        }
        buf.clear();
    }
    Ok(())
}

fn tail(path: &[String], n: usize) -> Vec<&str> {
    path[path.len().saturating_sub(n)..]
        .iter()
        .map(String::as_str)
        .collect()
}

fn int(text: &str, what: &str) -> Result<i64> {
    number(text)
        .filter(|n| n.is_finite())
        .map(|n| n as i64)
        .ok_or_else(|| decode(format!("bad `{what}`: {text}")))
}

pub(crate) fn updates(body: &[u8]) -> Result<Updates> {
    let (mut start, mut end, mut step) = (None, None, None);
    let mut legend = Vec::new();
    let mut rows: Vec<UpdateRow> = Vec::new();
    walk(body, |path, text| {
        let Some(text) = text else {
            if tail(path, 2) == ["data", "row"] {
                rows.push(UpdateRow {
                    t: 0,
                    values: Vec::new(),
                });
            }
            return Ok(());
        };
        {
            match tail(path, 2).as_slice() {
                ["meta", "start"] => start = Some(int(text, "start")?),
                ["meta", "end"] => end = Some(int(text, "end")?),
                ["meta", "step"] => step = Some(int(text, "step")?),
                ["legend", "entry"] => legend.push(
                    SourceId::parse(text)
                        .ok_or_else(|| decode(format!("bad legend entry {text}")))?,
                ),
                ["row", "t"] => {
                    if let Some(row) = rows.last_mut() {
                        row.t = int(text, "t")?;
                    }
                }
                ["row", "v"] => {
                    if let Some(row) = rows.last_mut() {
                        row.values.push(number(text).unwrap_or(f64::NAN));
                    }
                }
                _ => {}
            }
            Ok(())
        }
    })?;
    Ok(Updates {
        start: start.ok_or_else(|| decode("no `start`"))?,
        end: end.ok_or_else(|| decode("no `end`"))?,
        step: step.ok_or_else(|| decode("no `step`"))?,
        legend,
        rows,
    })
}

pub(crate) fn rrd(body: &[u8]) -> Result<Rrd> {
    let (mut step, mut last_update) = (None, None);
    let mut sources = Vec::new();
    let mut archives: Vec<(Option<Cf>, i64, Vec<Vec<f64>>)> = Vec::new();
    walk(body, |path, text| {
        let Some(text) = text else {
            match tail(path, 2).as_slice() {
                ["rrd", "rra"] => archives.push((None, 1, Vec::new())),
                ["database", "row"] => {
                    if let Some(rra) = archives.last_mut() {
                        rra.2.push(Vec::new());
                    }
                }
                _ => {}
            }
            return Ok(());
        };
        {
            match tail(path, 2).as_slice() {
                ["rrd", "step"] => step = Some(int(text, "step")?),
                ["rrd", "lastupdate"] => {
                    last_update = Some(
                        number(text)
                            .filter(|n| n.is_finite())
                            .ok_or_else(|| decode("bad `lastupdate`"))?,
                    );
                }
                ["ds", "name"] if tail(path, 3)[0] == "rrd" => {
                    sources.push(text.trim().to_owned());
                }
                ["rra", "cf"] => {
                    if let Some(rra) = archives.last_mut() {
                        rra.0 = Cf::parse(text);
                    }
                }
                ["rra", "pdp_per_row"] => {
                    if let Some(rra) = archives.last_mut() {
                        rra.1 = int(text, "pdp_per_row")?;
                    }
                }
                ["row", "v"] => {
                    if let Some(row) = archives.last_mut().and_then(|a| a.2.last_mut()) {
                        row.push(number(text).unwrap_or(f64::NAN));
                    }
                }
                _ => {}
            }
            Ok(())
        }
    })?;
    Ok(Rrd {
        step: step.ok_or_else(|| decode("no `step`"))?,
        last_update: last_update.ok_or_else(|| decode("no `lastupdate`"))?,
        sources,
        archives: archives
            .into_iter()
            .filter_map(|(cf, pdp_per_row, rows)| {
                Some(Rra {
                    cf: cf?,
                    pdp_per_row,
                    rows,
                })
            })
            .collect(),
    })
}
