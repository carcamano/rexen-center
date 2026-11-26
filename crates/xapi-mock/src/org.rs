//! Folders, tags, custom fields and saved searches (F13) in the mock.
//!
//! Folders and custom field values live in `other_config` (the generic
//! `<class>.add_to_other_config` / `remove_from_other_config`); this module
//! adds what is left, as XAPI behaves:
//!
//! * `<class>.add_tags` / `remove_tags` for VMs, hosts, pools, SRs, VDIs
//!   and networks: the `tags` field is a set, so adding a tag already there
//!   or removing one that is not changes nothing;
//! * `pool.add_to_gui_config` (`MAP_DUPLICATE_KEY` when the key is already
//!   there) and `pool.remove_from_gui_config` (nothing when it is not),
//!   where XenCenter keeps custom field definitions and saved searches.

use serde_json::{Value, json};

use crate::{Db, Reply, api_error, str_param};

/// Whether `<class>.<call>` is handled here.
pub(crate) fn handles(class: &str, call: &str) -> bool {
    matches!(call, "add_tags" | "remove_tags")
        && matches!(
            class.to_lowercase().as_str(),
            "vm" | "host" | "pool" | "sr" | "vdi" | "network"
        )
        || (class == "pool" && matches!(call, "add_to_gui_config" | "remove_from_gui_config"))
}

impl Db {
    /// A tag or `gui_config` call.
    pub(crate) fn org_call(&mut self, class: &str, call: &str, params: &[Value]) -> Reply {
        let class = class.to_lowercase();
        let r = str_param(params, 1).to_owned();
        if self.rec(&class, &r).is_none() {
            return api_error("HANDLE_INVALID", &[&class, &r]);
        }
        match call {
            "add_tags" | "remove_tags" => {
                let tag = str_param(params, 2).to_owned();
                let present = self
                    .rec(&class, &r)
                    .and_then(|rec| rec.get("tags"))
                    .and_then(Value::as_array)
                    .is_some_and(|t| t.iter().any(|x| x.as_str() == Some(&tag)));
                let add = call == "add_tags";
                if present != add {
                    self.modify(&class, &r, |rec| {
                        let tags = rec.entry("tags").or_insert_with(|| json!([]));
                        if let Value::Array(list) = tags {
                            if add {
                                list.push(json!(tag));
                            } else {
                                list.retain(|x| x.as_str() != Some(&tag));
                            }
                        }
                    });
                }
                Ok(json!(""))
            }
            _ => {
                let key = str_param(params, 2).to_owned();
                let present = self
                    .rec(&class, &r)
                    .and_then(|rec| rec.get("gui_config"))
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.contains_key(&key));
                if call == "add_to_gui_config" {
                    if present {
                        return api_error("MAP_DUPLICATE_KEY", &["pool", "gui_config", &r, &key]);
                    }
                    let value = str_param(params, 3).to_owned();
                    self.modify(&class, &r, |rec| {
                        let map = rec.entry("gui_config").or_insert_with(|| json!({}));
                        if let Value::Object(m) = map {
                            m.insert(key, json!(value));
                        }
                    });
                } else if present {
                    self.modify(&class, &r, |rec| {
                        if let Some(Value::Object(m)) = rec.get_mut("gui_config") {
                            m.remove(&key);
                        }
                    });
                }
                Ok(json!(""))
            }
        }
    }
}
