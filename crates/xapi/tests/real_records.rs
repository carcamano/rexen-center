//! The generated record types against records of a real XCP-ng 8.3 host
//! (`tests/fixtures/xcp-ng-8.3-capture.json`, a sanitized read-only capture).
//!
//! Every record of every class must decode, and re-encoding it must give back
//! every field exactly: proof that field names, types and enum values of the
//! pinned datamodel match what the server sends.

use serde_json::{Map, Value};
use xapi::{XapiClass, XapiObject};

/// Decodes `records` as class `T` and returns the problems found.
fn check<T: XapiObject>(records: &Map<String, Value>) -> Vec<String> {
    let mut problems = Vec::new();
    for (opaque_ref, original) in records {
        let decoded: T::Record = match serde_json::from_value(original.clone()) {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!("{} {opaque_ref}: {e}", T::NAME));
                continue;
            }
        };
        let encoded = serde_json::to_value(&decoded).unwrap();
        for (field, value) in original.as_object().unwrap() {
            match encoded.get(field) {
                None => problems.push(format!("{}.{field}: not in the datamodel", T::NAME)),
                Some(v) if v != value => {
                    problems.push(format!("{}.{field}: {value} decoded as {v}", T::NAME))
                }
                Some(_) => {}
            }
        }
    }
    problems
}

macro_rules! check_cached {
    ($( ($marker:ident, $module:ident, $plural:ident) ),* $(,)?) => {
        fn check_class(class: &str, records: &Map<String, Value>) -> Option<Vec<String>> {
            $(
                if <xapi::$marker as XapiClass>::is_class(class) {
                    return Some(check::<xapi::$marker>(records));
                }
            )*
            None
        }
    };
}
xapi::for_each_cached_class!(check_cached);

#[test]
fn every_real_record_round_trips() {
    let db = xapi_mock::capture_records();
    let mut problems = Vec::new();
    let mut checked = 0;
    for (class, records) in db.as_object().unwrap() {
        if class.starts_with('_') {
            continue;
        }
        let records = records.as_object().unwrap();
        checked += records.len();
        let found = match class.as_str() {
            // Removed from the datamodel (so not cached) but still sent.
            "vbd_metrics" => check::<xapi::VbdMetrics>(records),
            "vif_metrics" => check::<xapi::VifMetrics>(records),
            _ => check_class(class, records)
                .unwrap_or_else(|| vec![format!("class {class} is not cached")]),
        };
        problems.extend(found);
    }
    assert!(checked > 250, "fixture too small: {checked}");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn real_enum_values_are_all_known() {
    let db = xapi_mock::capture_records();
    for vm in db["vm"].as_object().unwrap().values() {
        let vm: xapi::VmRecord = serde_json::from_value(vm.clone()).unwrap();
        assert!(!vm.power_state.is_unknown());
        // "sysprep" was unknown to F1's hand-written enum.
        assert!(vm.allowed_operations.iter().all(|op| !op.is_unknown()));
        assert!(!vm.domain_type.is_unknown());
    }
    for pif in db["pif"].as_object().unwrap().values() {
        let pif: xapi::PifRecord = serde_json::from_value(pif.clone()).unwrap();
        assert!(!pif.ip_configuration_mode.is_unknown());
        assert!(!pif.primary_address_type.is_unknown());
    }
}

#[test]
fn datetimes_and_maps_decode() {
    let db = xapi_mock::capture_records();
    let host = db["host"].as_object().unwrap().values().next().unwrap();
    let host: xapi::HostRecord = serde_json::from_value(host.clone()).unwrap();
    assert_eq!((host.api_version_major, host.api_version_minor), (2, 21));
    assert_eq!(host.software_version["product_brand"], "XCP-ng");
    let metrics = db["host_metrics"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    let metrics: xapi::HostMetricsRecord = serde_json::from_value(metrics.clone()).unwrap();
    assert!(metrics.last_updated.unix_seconds().unwrap() > 1_700_000_000);
    assert!(metrics.memory_total > metrics.memory_free);
}
