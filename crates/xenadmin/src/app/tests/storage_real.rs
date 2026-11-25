//! F8 on a real host, read only, through the real window code (an
//! ignored test, run with the `XENADMIN_REAL_*` variables of the other
//! real-host tests and `XENADMIN_REAL_LOG` outside the repository).
//!
//! For every SR the tree shows it draws the General and Storage tabs, the
//! Storage menu (only to read which operations it offers, closed with
//! Escape), and the Properties dialog (closed with Cancel), and checks them
//! against the cache's records; then it opens the New SR wizard's type
//! page and cancels. It never presses an operation's button: no SR is
//! created, probed, scanned, repaired, detached, forgotten, destroyed or
//! trimmed, no PBD is touched and nothing is saved. At the end the
//! operation history must hold the connection only.

use egui_kittest::Harness;
use xen_core::objects;
use xen_core::storage::{self, SrOp};

use super::provisioning::{last_enabled, press, select, shows, tab};
use super::storage::{open_wizard, select_sr, storage_menu_enabled};
use super::*;
use crate::sr_strings::sr_strings;

fn note(log: &mut String, line: String) {
    println!("{line}");
    log.push_str(&line);
    log.push('\n');
}

#[test]
#[ignore = "needs a real XCP-ng host (XENADMIN_REAL_* variables)"]
fn real_f8_srs_read_only() {
    let log_path = std::env::var("XENADMIN_REAL_LOG").expect("set XENADMIN_REAL_LOG");
    let runtime = runtime();
    let mut h: Harness<'_, XenAdminApp> = real_host_harness(&runtime);
    let mut log = String::new();
    let s = sr_strings();
    let t = tab_strings();
    let state = h.state().backend.state();
    let conn = state.connections[0].clone();
    let m = conn.model.clone();
    let pool = m.pool().map(|(r, _)| r.clone()).expect("a pool");
    note(
        &mut log,
        format!(
            "hosts {} SRs {} PBDs {} SMs {}",
            m.hosts.len(),
            m.srs.len(),
            m.pbds.len(),
            m.sms.len()
        ),
    );

    let mut srs: Vec<_> = m
        .srs
        .iter()
        .filter(|(_, sr)| objects::sr_is_shown(sr, false) && !objects::is_tools_sr(sr))
        .map(|(r, sr)| (r.clone(), sr.clone()))
        .collect();
    srs.sort_by(|a, b| a.1.name_label.cmp(&b.1.name_label));
    for (r, sr) in &srs {
        select_sr(&mut h, r.as_str());
        // The tab chosen last is kept from one SR to the next.
        tab(&mut h, t.tab_general);
        let type_label = tabs::general::sr_type_label(&m, &sr.r#type);
        // General tab: type, size, UUID, state of each PBD.
        h.get_by_label(&type_label);
        h.get_by_label(&sr.uuid);
        let sized = sr.content_type != "iso" && sr.r#type != "udev";
        if sized {
            h.get_by_label(&tabs::general::sr_size_text(sr));
        }
        let attached = sr
            .pbds
            .iter()
            .filter_map(|p| m.get(p))
            .filter(|p| p.currently_attached)
            .count();
        note(
            &mut log,
            format!(
                "SR {:?} type {} ({type_label}) content {:?} shared {} PBDs {}/{} attached{}",
                sr.name_label,
                sr.r#type,
                sr.content_type,
                sr.shared,
                attached,
                sr.pbds.len(),
                if sized {
                    format!(" size {:?}", tabs::general::sr_size_text(sr))
                } else {
                    String::new()
                }
            ),
        );

        // Storage tab: every disk shown, Rescan offered as the original
        // offers it (never pressed).
        tab(&mut h, t.tab_storage);
        let shown: Vec<String> = sr
            .vdis
            .iter()
            .filter_map(|v| m.get(v))
            .filter(|v| objects::vdi_is_shown(v, false))
            .map(|v| objects::vdi_name(&m, v))
            .collect();
        for name in &shown {
            assert!(shows(&h, name), "{name} on the Storage tab");
        }
        let rescan = storage::sr_op_available(&m, r, sr, SrOp::Rescan, false);
        assert_eq!(
            last_enabled(&h, s.rescan),
            rescan,
            "Rescan of {:?}",
            sr.name_label
        );
        note(
            &mut log,
            format!("   disks shown {} rescan offered {rescan}", shown.len()),
        );

        // The Storage menu's operations, as the original's rules say.
        let mut offered = Vec::new();
        for (label, op) in [
            (s.repair, SrOp::Repair),
            (s.set_default, SrOp::SetDefault),
            (s.trim, SrOp::Trim),
            (s.detach, SrOp::Detach),
            (s.reattach, SrOp::Reattach),
            (s.forget, SrOp::Forget),
            (s.destroy, SrOp::Destroy),
        ] {
            let expected = storage::sr_op_available(&m, r, sr, op, false);
            let enabled = storage_menu_enabled(&mut h, label);
            assert_eq!(enabled, expected, "{label} on {:?}", sr.name_label);
            if enabled {
                offered.push(label);
            }
        }
        let problems: Vec<String> = [SrOp::Detach, SrOp::Forget, SrOp::Destroy, SrOp::Trim]
            .into_iter()
            .filter_map(|op| {
                storage::sr_op_problem(&m, sr, op, false)
                    .map(|p| format!("{op:?}: {}", storage_ui::problem_text(p)))
            })
            .collect();
        note(
            &mut log,
            format!("   menu offers {offered:?}; not offered because {problems:?}"),
        );

        // Properties: the record's values; Cancel saves nothing.
        click_last(&mut h, s.properties);
        step_until(&mut h, "properties", |h| {
            h.query_by_label(&fill(s.properties_title, &[("name", &sr.name_label)]))
                .is_some()
        });
        let (name, description, tags, caching, alerts) = {
            let d = h.state_mut().storage.properties().expect("open");
            (
                d.name.clone(),
                d.description.clone(),
                d.tags.clone(),
                d.read_caching,
                d.perfmon.is_some(),
            )
        };
        assert_eq!(name, sr.name_label);
        assert_eq!(description, sr.name_description);
        assert_eq!(tags, sr.tags.join(", "));
        assert_eq!(
            caching,
            storage::supports_read_caching(&m, sr).then(|| storage::read_caching_enabled(&m, sr))
        );
        assert_eq!(shows(&h, s.tab_read_caching), caching.is_some());
        note(
            &mut log,
            format!(
                "   properties: name/description/tags as recorded, read caching {caching:?}, alerts page {alerts}"
            ),
        );
        // Cancel (a window just opened may take the first click to come to
        // the front); nothing is saved.
        for _ in 0..3 {
            if h.state_mut().storage.properties().is_none() {
                break;
            }
            press(&mut h, s.cancel);
        }
        assert!(h.state_mut().storage.properties().is_none());
    }

    // The wizard's type page offers what the host's SM plugins allow;
    // cancelled before any probe.
    select(&mut h, Selection::Pool(conn.id, pool));
    open_wizard(&mut h);
    let mut offered = Vec::new();
    for (label, kind) in [
        (s.type_nfs, storage::SrKind::Nfs),
        (s.type_iscsi, storage::SrKind::Iscsi),
        (s.type_smb, storage::SrKind::Smb),
        (s.type_local, storage::SrKind::Local(storage::LocalFs::Ext)),
        (s.type_nfs_iso, storage::SrKind::NfsIso),
        (s.type_smb_iso, storage::SrKind::SmbIso),
    ] {
        let available = storage::kind_available(&m, kind, false);
        assert_eq!(shows(&h, label), available, "{label}");
        if available {
            offered.push(label);
        }
    }
    note(&mut log, format!("New SR wizard offers {offered:?}"));
    for _ in 0..3 {
        if h.state().storage.wizard.is_none() {
            break;
        }
        press(&mut h, s.cancel);
    }
    assert!(h.state().storage.wizard.is_none());

    // Nothing but the connection ran.
    let ops = h.state().backend.state().operations.clone();
    let others: Vec<_> = ops
        .iter()
        .filter(|o| {
            !matches!(
                o.kind,
                xen_core::OperationKind::Connect | xen_core::OperationKind::Reconnect
            )
        })
        .map(|o| format!("{:?}", o.kind))
        .collect();
    assert!(others.is_empty(), "operations ran: {others:?}");
    note(
        &mut log,
        format!(
            "history: {} connection entries, no other operation",
            ops.len()
        ),
    );
    std::fs::write(&log_path, log).expect("write the log");
}
