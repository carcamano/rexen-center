//! The networking operations (see [`crate::network`] for the requests and
//! when each is offered).
//!
//! Each follows the original console's action, call for call:
//!
//! * **New network** (`NetworkAction`, `CreateChinAction`): `network.create`
//!   (`other_config["automatic"]`, the MTU), then for an external network
//!   `pool.create_VLAN_from_PIF` on the chosen NIC, for a cross-server
//!   private network a `tunnel` on each management interface of the chosen
//!   network.
//!
//!   The original leaves the new network behind when the VLAN or a tunnel
//!   cannot be made. Here the half-made network is undone (its tunnels
//!   destroyed, then the network), so a failure leaves nothing.
//! * **Remove network** (`NetworkAction` destroy): each PIF goes first (a
//!   tunnel access PIF with its tunnel, a VLAN interface with its VLAN, a
//!   physical PIF forgotten), then the network.
//! * **New bond** (`CreateBondAction`): a network marked
//!   `XenCenterCreateInProgress` (the mark removed at once), then on every
//!   host, the coordinator last, `Bond.create` over the NICs with the
//!   chosen devices (LACP with its `hashing_algorithm`); then the names of
//!   secondary interfaces move from the members to the bond. On failure the
//!   names move back, the bonds made are destroyed and the network too.
//! * **Delete bond** (`DestroyBondAction`): the bond on every host; their
//!   secondary interface names move to the first member, each bond is
//!   destroyed, then the network; the first failure is reported once all
//!   were tried.
//! * **Properties** (`EditNetworkPage`): name, description, tags,
//!   `automatic`, MTU, bond mode and LACP hashing; a new NIC or VLAN (or
//!   `<internal>`) replaces the network's PIFs (`NetworkAction` update: the
//!   tag is checked, the PIFs destroyed, the VLAN made); a new MTU alone
//!   replugs the network (`UnplugPlugNetworkAction`: running VMs'
//!   interfaces and the PIFs unplugged and plugged again, every step tried).
//! * **Rescan** (`RescanPIFsAction`): `PIF.scan`.
//! * **Configure IP addresses** (`ChangeNetworkingAction` with
//!   `NetworkingActionHelpers`): interfaces brought up on the other hosts,
//!   then the dialog's host; the management interface moved (one host:
//!   `host.management_reconfigure`; a pool: `pool.management_reconfigure`,
//!   then waiting for the members to recover); then interfaces brought
//!   down. Bringing up sets `disallow_unplug` and the purpose, then
//!   `PIF.reconfigure_ip` (in a pool each host takes the next address of
//!   the range) and plugs a secondary interface; bringing down clears them
//!   and the address (not on a clustering interface). Renaming a secondary
//!   interface only is `SetSecondaryManagementPurposeAction`. The
//!   original's GFS2/clustering handling is not ported (no clustering yet).
//! * **Interfaces of a VM** (`CreateVIFAction`, `DeleteVIFAction`,
//!   `UpdateVIFAction`, `PlugVIFAction`, `UnplugVIFAction`): a new
//!   interface of a running VM is hot-plugged when the server allows it,
//!   else the user is told to restart the VM; one is unplugged before it is
//!   destroyed; Properties destroys and makes it again.

use std::collections::HashMap;
use std::sync::Arc;

use xapi::error::codes;
use xapi::{
    Bond, Host, IpConfigurationMode, Network, NetworkRecord, Pif, PifRecord, Ref, Session, Tunnel,
    Vif, VifOperations, VifRecord, Vm, VmPowerState,
};

use crate::actions::ActionRequest;
use crate::backend::{ConnectionId, OperationId, Shared};
use crate::model::ConnectionModel;
use crate::network::{
    self, IpChange, NetworkChanges, NewBondSpec, NewNetworkKind, NewNetworkSpec, PifChange,
    PifConfig, VifSpec, errors,
};
use crate::objects;
use crate::operations::{OperationNotice, OperationStep, Span, TaskOutcome, run_task};
use crate::storage_run::{done, made, model, outcome, step};

/// Like `?` for plain API calls returning a [`TaskOutcome`] on error.
macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Err(TaskOutcome::Failed(e)),
        }
    };
}

/// Runs an F9 request.
pub(crate) async fn run(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    session: &Session,
    request: &ActionRequest,
) -> TaskOutcome {
    let s = session;
    match request {
        ActionRequest::NewNetwork(spec) => {
            outcome(new_network(shared, connection, op, s, spec).await)
        }
        ActionRequest::NewBond(spec) => outcome(create_bond(shared, connection, op, s, spec).await),
        ActionRequest::RemoveNetwork { network } => {
            outcome(remove_network(shared, connection, op, s, network).await)
        }
        ActionRequest::DestroyBond { bond } => {
            outcome(destroy_bond(shared, connection, op, s, bond).await)
        }
        ActionRequest::SetNetworkProperties { network, changes } => {
            outcome(set_properties(shared, connection, op, s, network, changes).await)
        }
        ActionRequest::RescanPifs { host } => {
            step(shared, op, OperationStep::ScanningNics);
            run_task(
                shared,
                op,
                s,
                xapi::pif::async_scan(s, host),
                Span(0.4, 1.0),
                false,
            )
            .await
        }
        ActionRequest::ChangeNetworking(change) => {
            outcome(change_networking(shared, connection, op, s, change).await)
        }
        ActionRequest::RenameInterfaces { pool, pifs } => {
            outcome(rename_interfaces(shared, connection, s, *pool, pifs).await)
        }
        ActionRequest::NewVif { vm, spec } => {
            outcome(create_vif(shared, connection, op, s, vm, spec, None, Span::FULL).await)
        }
        ActionRequest::DeleteVif { vif } => {
            outcome(delete_vif(shared, connection, op, s, vif, Span::FULL).await)
        }
        ActionRequest::UpdateVif { vif, spec } => {
            outcome(update_vif(shared, connection, op, s, vif, spec).await)
        }
        ActionRequest::PlugVif { vif } => outcome(plug_vif(shared, connection, s, vif, true).await),
        ActionRequest::UnplugVif { vif } => {
            outcome(plug_vif(shared, connection, s, vif, false).await)
        }
        _ => unreachable!("not a networking request"),
    }
}

fn no_coordinator() -> TaskOutcome {
    TaskOutcome::Failed(network::client_error(errors::NO_COORDINATOR, &[]))
}

fn progress(shared: &Shared, op: OperationId, value: f64) {
    shared.update_operation(op, |o| o.progress = Some(value.clamp(0.0, 1.0)));
}

/// The record `network.create` gets.
fn network_record(
    name: &str,
    description: &str,
    auto_plug: bool,
    mtu: i64,
    extra: &[(&str, &str)],
) -> NetworkRecord {
    let mut other_config = HashMap::from([(
        "automatic".to_owned(),
        if auto_plug { "true" } else { "false" }.to_owned(),
    )]);
    for (k, v) in extra {
        other_config.insert((*k).to_owned(), (*v).to_owned());
    }
    NetworkRecord {
        name_label: name.to_owned(),
        name_description: description.to_owned(),
        mtu,
        other_config,
        managed: true,
        // Empty enum values are refused by XAPI.
        default_locking_mode: xapi::NetworkDefaultLockingMode::Unlocked,
        ..Default::default()
    }
}

/// Destroys `network` as part of a cleanup; never fails.
async fn destroy_network_quietly(s: &Session, network: &Ref<Network>) {
    if let Err(e) = xapi::network::destroy(s, network).await {
        tracing::warn!(error = %e, "destroying the network as part of a cleanup failed");
    }
}

// ---- New network ------------------------------------------------------------

async fn new_network(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewNetworkSpec,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    step(shared, op, OperationStep::CreatingNetwork);
    let record = network_record(
        &spec.name,
        &spec.description,
        spec.auto_plug,
        spec.mtu.unwrap_or(network::MTU_DEFAULT),
        &[],
    );
    let created = made::<Network>(
        shared,
        op,
        s,
        xapi::network::async_create(s, &record),
        Span(0.0, 0.4),
    )
    .await?;
    let finished = match &spec.kind {
        NewNetworkKind::Internal => Ok(()),
        NewNetworkKind::External { pif, vlan } => {
            if network::coordinator(&m).is_none() {
                Err(no_coordinator())
            } else {
                done(
                    run_task(
                        shared,
                        op,
                        s,
                        xapi::pool::async_create_vlan_from_pif(s, pif, &created, *vlan),
                        Span(0.4, 1.0),
                        false,
                    )
                    .await,
                )
            }
        }
        NewNetworkKind::CrossServer { interface } => {
            make_tunnels(shared, op, s, &m, interface, &created).await
        }
    };
    if let Err(failure) = finished {
        step(shared, op, OperationStep::UndoingNetwork);
        destroy_network_quietly(s, &created).await;
        return Err(failure);
    }
    Ok(())
}

/// `CreateChinAction`: a tunnel on each management interface of
/// `interface`. On failure the tunnels made are destroyed.
async fn make_tunnels(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    m: &ConnectionModel,
    interface: &Ref<Network>,
    network: &Ref<Network>,
) -> Result<(), TaskOutcome> {
    let transport: Vec<Ref<Pif>> = m
        .networks
        .get(interface)
        .map(|n| {
            network::network_pifs(m, n)
                .into_iter()
                .filter(|(_, p)| network::pif_is_management_interface(m, p, true))
                .map(|(r, _)| r.clone())
                .collect()
        })
        .unwrap_or_default();
    let share = 0.6 / transport.len().max(1) as f64;
    let mut tunnels: Vec<Ref<Tunnel>> = Vec::new();
    for (i, pif) in transport.iter().enumerate() {
        let from = 0.4 + share * i as f64;
        let made = made::<Tunnel>(
            shared,
            op,
            s,
            xapi::tunnel::async_create(s, pif, network, None),
            Span(from, from + share),
        )
        .await;
        match made {
            Ok(t) => tunnels.push(t),
            Err(failure) => {
                for t in &tunnels {
                    if let Err(e) = xapi::tunnel::destroy(s, t).await {
                        tracing::warn!(error = %e, "destroying a tunnel as part of a cleanup failed");
                    }
                }
                return Err(failure);
            }
        }
    }
    Ok(())
}

// ---- Remove network ---------------------------------------------------------

/// `NetworkAction.destroyPIFs`.
async fn destroy_pifs(s: &Session, pifs: &[(Ref<Pif>, PifRecord)]) -> Result<(), TaskOutcome> {
    for (r, pif) in pifs {
        if network::pif_is_tunnel_access(pif) {
            for tunnel in &pif.tunnel_access_pif_of {
                tri!(xapi::tunnel::destroy(s, tunnel).await);
            }
        } else if !pif.physical {
            if pif.vlan != -1 {
                tri!(xapi::vlan::destroy(s, &pif.vlan_master_of).await);
            }
            if let Some(sriov) = pif.sriov_logical_pif_of.first() {
                tri!(xapi::network_sriov::destroy(s, sriov).await);
            }
        } else {
            tri!(xapi::pif::forget(s, r).await);
        }
    }
    Ok(())
}

fn pifs_of(m: &ConnectionModel, network: &Ref<Network>) -> Vec<(Ref<Pif>, PifRecord)> {
    m.networks
        .get(network)
        .map(|n| {
            network::network_pifs(m, n)
                .into_iter()
                .map(|(r, p)| (r.clone(), p.clone()))
                .collect()
        })
        .unwrap_or_default()
}

async fn remove_network(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    network: &Ref<Network>,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    step(shared, op, OperationStep::RemovingNetwork);
    destroy_pifs(s, &pifs_of(&m, network)).await?;
    progress(shared, op, 0.5);
    done(
        run_task(
            shared,
            op,
            s,
            xapi::network::async_destroy(s, network),
            Span(0.5, 1.0),
            false,
        )
        .await,
    )
}

// ---- Bonds -------------------------------------------------------------------

/// `NetworkingActionHelpers.MoveManagementInterfaceName`: when `from` is a
/// secondary interface, its name moves to `to`.
async fn move_management_name(s: &Session, from: &Ref<Pif>, to: &Ref<Pif>) -> xapi::Result<()> {
    let mut from_config = xapi::pif::get_other_config(s, from).await?;
    let Some(purpose) = from_config
        .get(network::MANAGEMENT_PURPOSE)
        .filter(|p| !p.is_empty())
        .cloned()
    else {
        return Ok(());
    };
    let mut to_config = xapi::pif::get_other_config(s, to).await?;
    to_config.insert(network::MANAGEMENT_PURPOSE.to_owned(), purpose);
    xapi::pif::set_other_config(s, to, &to_config).await?;
    from_config.remove(network::MANAGEMENT_PURPOSE);
    xapi::pif::set_other_config(s, from, &from_config).await
}

/// A bond made on one host: the bond, its interface, its members.
struct MadeBond {
    bond: Ref<Bond>,
    interface: Ref<Pif>,
    members: Vec<Ref<Pif>>,
}

async fn create_bond(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    spec: &NewBondSpec,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some(coordinator) = network::coordinator(&m).cloned() else {
        return Err(no_coordinator());
    };
    // `GetHostsCoordinatorLast`.
    let mut per_host = network::pifs_on_all_hosts(&m, &spec.pifs);
    if let Some(i) = per_host.iter().position(|(h, _)| *h == coordinator) {
        let c = per_host.remove(i);
        per_host.push(c);
    }
    let inc = 1.0 / (per_host.len() * 2 + 1) as f64;
    step(shared, op, OperationStep::CreatingNetwork);
    let record = network_record(
        &spec.name,
        "",
        spec.auto_plug,
        spec.mtu,
        &[(network::CREATE_IN_PROGRESS, "true")],
    );
    let net = made::<Network>(
        shared,
        op,
        s,
        xapi::network::async_create(s, &record),
        Span(0.0, inc),
    )
    .await?;
    let mut bonds: Vec<MadeBond> = Vec::new();
    let made_all = async {
        tri!(xapi::network::remove_from_other_config(s, &net, network::CREATE_IN_PROGRESS).await);
        let mut lo = inc;
        for (host, pifs) in &per_host {
            let members: Vec<Ref<Pif>> = pifs
                .iter()
                .filter(|p| m.get(*p).is_some_and(|r| r.physical))
                .cloned()
                .collect();
            let properties: HashMap<String, String> = match (&spec.mode, spec.hashing) {
                (xapi::BondMode::Lacp, Some(h)) => {
                    HashMap::from([("hashing_algorithm".to_owned(), h.as_str().to_owned())])
                }
                _ => HashMap::new(),
            };
            // Bonding the coordinator's management NIC drops the link for a
            // moment (`Connection.ExpectDisruption`).
            let disrupts =
                host == &coordinator && network::contains_primary_management(&m, &members);
            let task = xapi::bond::async_create(
                s,
                &net,
                &members,
                "",
                Some(&spec.mode),
                Some(&properties),
            );
            let (result, value) = crate::operations::run_task_result(
                shared,
                op,
                s,
                task,
                Span(lo, lo + inc),
                disrupts,
            )
            .await;
            lo += inc;
            done(result)?;
            let bond: Ref<Bond> = if value.starts_with("OpaqueRef:") {
                Ref::new(&value)
            } else {
                // The link went away while the task ran: find the bond.
                let found = find_new_bond(s, &net, host).await;
                match found {
                    Some(b) => b,
                    None => {
                        return Err(TaskOutcome::Failed(network::client_error(
                            errors::BOND_GONE,
                            &[],
                        )));
                    }
                }
            };
            let interface = tri!(xapi::bond::get_master(s, &bond).await);
            bonds.push(MadeBond {
                bond,
                interface,
                members,
            });
        }
        for b in &bonds {
            for member in &b.members {
                tri!(move_management_name(s, member, &b.interface).await);
            }
            lo += inc;
            progress(shared, op, lo);
        }
        Ok(())
    }
    .await;
    if let Err(failure) = made_all {
        step(shared, op, OperationStep::UndoingNetwork);
        for b in &bonds {
            // `RevertManagementInterfaces`.
            let back = match xapi::bond::get_primary_slave(s, &b.bond).await {
                Ok(p) if !p.is_null() => Some(p),
                _ => b.members.first().cloned(),
            };
            if let Some(back) = back
                && let Err(e) = move_management_name(s, &b.interface, &back).await
            {
                tracing::warn!(error = %e, "moving an interface name back failed");
            }
            if let Err(e) = xapi::bond::destroy(s, &b.bond).await {
                tracing::warn!(error = %e, "destroying a bond as part of a cleanup failed");
            }
        }
        destroy_network_quietly(s, &net).await;
        return Err(failure);
    }
    Ok(())
}

/// The bond of `network` on `host`, read from the server.
async fn find_new_bond(s: &Session, network: &Ref<Network>, host: &Ref<Host>) -> Option<Ref<Bond>> {
    for pif in xapi::network::get_pifs(s, network).await.ok()? {
        let Ok(record) = xapi::pif::get_record(s, &pif).await else {
            continue;
        };
        if &record.host == host
            && let Some(b) = record.bond_master_of.first()
        {
            return Some(b.clone());
        }
    }
    None
}

/// A host's bond to destroy: the bond, its interface, the member its
/// names move to, and whether it carries the management interface.
type HostBond = (Ref<Bond>, Ref<Pif>, Option<Ref<Pif>>, bool);

async fn destroy_bond(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    bond: &Ref<Bond>,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some(given) = m.get(bond).map(|b| (**b).clone()) else {
        return Err(TaskOutcome::Failed(network::client_error(
            errors::BOND_GONE,
            &[],
        )));
    };
    // Every host's equivalent bond, with its interface and first member.
    let mut found: Vec<HostBond> = Vec::new();
    for (host, _) in objects::sorted_hosts(&m) {
        let Some((b_ref, b)) = network::find_bond(&m, host, &given) else {
            continue;
        };
        let mut members: Vec<(&Ref<Pif>, &PifRecord)> = b
            .slaves
            .iter()
            .filter_map(|p| m.pifs.get_key_value(p))
            .map(|(r, p)| (r, &**p))
            .collect();
        members.sort_by(|(_, a), (_, c)| {
            objects::natural_cmp(&objects::pif_name(&m, a), &objects::pif_name(&m, c))
        });
        let first = Some(b.primary_slave.clone())
            .filter(|p| m.pifs.contains_key(p))
            .or_else(|| members.first().map(|(r, _)| (*r).clone()));
        let management = m.get(&b.master).is_some_and(|p| p.management);
        found.push((b_ref.clone(), b.master.clone(), first, management));
    }
    let net = m.get(&given.master).map(|p| p.network.clone());
    let incr = 0.5 / found.len().max(1) as f64;
    for (_, interface, first, _) in &found {
        if let Some(first) = first
            && let Err(e) = move_management_name(s, interface, first).await
            && !e.is_unreachable()
        {
            return Err(TaskOutcome::Failed(e));
        }
        let p = shared
            .state()
            .operations
            .iter()
            .find(|o| o.id == op)
            .and_then(|o| o.progress)
            .unwrap_or(0.0);
        progress(shared, op, p + incr);
    }
    progress(shared, op, 0.5);
    let mut first_error: Option<TaskOutcome> = None;
    let inc = 0.4 / found.len().max(1) as f64;
    for (i, (b, _, _, management)) in found.iter().enumerate() {
        let from = 0.5 + inc * i as f64;
        let result = run_task(
            shared,
            op,
            s,
            xapi::bond::async_destroy(s, b),
            Span(from, from + inc),
            *management,
        )
        .await;
        if !matches!(result, TaskOutcome::Succeeded) {
            tracing::error!("failed to destroy a bond");
            first_error.get_or_insert(result);
        }
    }
    progress(shared, op, 0.9);
    if let Some(net) = net
        && let Err(e) = xapi::network::destroy(s, &net).await
    {
        tracing::error!(error = %e, "failed to destroy the bond's network");
        first_error.get_or_insert(TaskOutcome::Failed(e));
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---- Properties -------------------------------------------------------------

async fn set_properties(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    net: &Ref<Network>,
    changes: &NetworkChanges,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    if let Some(name) = &changes.name {
        tri!(xapi::network::set_name_label(s, net, name).await);
    }
    if let Some(description) = &changes.description {
        tri!(xapi::network::set_name_description(s, net, description).await);
    }
    if let Some(tags) = &changes.tags {
        tri!(xapi::network::set_tags(s, net, tags).await);
    }
    if let Some(auto) = changes.auto_plug {
        tri!(xapi::network::remove_from_other_config(s, net, "automatic").await);
        tri!(
            xapi::network::add_to_other_config(
                s,
                net,
                "automatic",
                if auto { "true" } else { "false" }
            )
            .await
        );
    }
    if let Some(mtu) = changes.mtu {
        tri!(xapi::network::set_mtu(s, net, mtu).await);
    }
    progress(shared, op, 0.2);
    let bonds: Vec<Ref<Bond>> = m
        .networks
        .get(net)
        .map(|n| {
            network::network_bonds(&m, n)
                .into_iter()
                .map(|(r, _)| r.clone())
                .collect()
        })
        .unwrap_or_default();
    if let Some(mode) = &changes.bond_mode {
        for b in &bonds {
            done(
                run_task(
                    shared,
                    op,
                    s,
                    xapi::bond::async_set_mode(s, b, mode),
                    Span(0.2, 0.3),
                    false,
                )
                .await,
            )?;
        }
    }
    if let Some(hashing) = changes.hashing {
        for b in &bonds {
            let start = xapi::bond::async_set_property(s, b, "hashing_algorithm", hashing.as_str());
            done(run_task(shared, op, s, start, Span(0.3, 0.4), false).await)?;
        }
    }
    if let Some(change) = &changes.pifs {
        step(shared, op, OperationStep::UpdatingNetwork);
        if let PifChange::External { pif, vlan } = change {
            // "Before we do a destroy check the vlan tag is not in use".
            let device = m.get(pif).map(|p| p.device.clone()).unwrap_or_default();
            if m.pifs
                .values()
                .any(|p| p.vlan == *vlan && p.device == device)
            {
                return Err(TaskOutcome::Failed(network::client_error(
                    errors::VLAN_EXISTS,
                    &[&device, &vlan.to_string()],
                )));
            }
        }
        destroy_pifs(s, &pifs_of(&m, net)).await?;
        progress(shared, op, 0.7);
        if let PifChange::External { pif, vlan } = change {
            if network::coordinator(&m).is_none() {
                return Err(no_coordinator());
            }
            let start = xapi::pool::async_create_vlan_from_pif(s, pif, net, *vlan);
            done(run_task(shared, op, s, start, Span(0.7, 1.0), false).await)?;
        }
    } else if changes.replug() {
        replug(shared, op, s, &m, net).await?;
    }
    Ok(())
}

/// `UnplugPlugNetworkAction`: the attached interfaces of running VMs and
/// the attached PIFs are unplugged, then plugged again; every step is
/// tried and the first failure reported.
async fn replug(
    shared: &Shared,
    op: OperationId,
    s: &Session,
    m: &ConnectionModel,
    net: &Ref<Network>,
) -> Result<(), TaskOutcome> {
    step(shared, op, OperationStep::RefreshingNetworks);
    let Some(record) = m.networks.get(net) else {
        return Ok(());
    };
    let pifs: Vec<Ref<Pif>> = network::network_pifs(m, record)
        .into_iter()
        .filter(|(_, p)| p.currently_attached)
        .map(|(r, _)| r.clone())
        .collect();
    let vifs: Vec<Ref<Vif>> = record
        .vifs
        .iter()
        .filter_map(|v| m.vifs.get_key_value(v))
        .filter(|(_, v)| v.currently_attached)
        .filter(|(_, v)| {
            m.vms
                .get(&v.vm)
                .is_some_and(|vm| vm.power_state == VmPowerState::Running)
        })
        .map(|(r, _)| r.clone())
        .collect();
    let steps = (pifs.len() * 2 + vifs.len() * 2).max(1) as f64;
    let mut done_steps = 0.0;
    let mut first: Option<xapi::Error> = None;
    let mut note = |r: xapi::Result<()>| {
        if let Err(e) = r {
            tracing::error!(error = %e, "refreshing the network");
            first.get_or_insert(e);
        }
    };
    for v in &vifs {
        note(xapi::vif::unplug(s, v).await);
        done_steps += 1.0;
        progress(shared, op, 0.1 + 0.9 * done_steps / steps);
    }
    for p in &pifs {
        note(xapi::pif::unplug(s, p).await);
        done_steps += 1.0;
        progress(shared, op, 0.1 + 0.9 * done_steps / steps);
    }
    for p in &pifs {
        note(xapi::pif::plug(s, p).await);
        done_steps += 1.0;
        progress(shared, op, 0.1 + 0.9 * done_steps / steps);
    }
    for v in &vifs {
        note(xapi::vif::plug(s, v).await);
        done_steps += 1.0;
        progress(shared, op, 0.1 + 0.9 * done_steps / steps);
    }
    match first {
        Some(e) => Err(TaskOutcome::Failed(e)),
        None => Ok(()),
    }
}

// ---- IP configuration -------------------------------------------------------

/// The other PIFs of `pif`'s network: on `pif`'s host (`this_host`) or on
/// the other hosts (`ForSomeHosts`).
fn some_hosts(m: &ConnectionModel, pif: &Ref<Pif>, this_host: bool) -> Vec<(Ref<Pif>, PifRecord)> {
    let Some(record) = m.get(pif) else {
        return Vec::new();
    };
    let Some(net) = m.networks.get(&record.network) else {
        tracing::warn!("network has gone away");
        return Vec::new();
    };
    network::network_pifs(m, net)
        .into_iter()
        .filter(|(_, p)| (p.host == record.host) == this_host)
        .map(|(r, p)| (r.clone(), p.clone()))
        .collect()
}

/// Saves `disallow_unplug` and the management purpose of `pif` (a PIF
/// clone's `SaveChanges`).
async fn save_purpose(
    s: &Session,
    pif: &Ref<Pif>,
    disallow_unplug: bool,
    purpose: Option<&str>,
) -> xapi::Result<()> {
    let current = xapi::pif::get_record(s, pif).await?;
    if current.disallow_unplug != disallow_unplug {
        xapi::pif::set_disallow_unplug(s, pif, disallow_unplug).await?;
    }
    if network::management_purpose(&current) != purpose {
        let mut config = current.other_config.clone();
        match purpose {
            Some(p) => config.insert(network::MANAGEMENT_PURPOSE.to_owned(), p.to_owned()),
            None => config.remove(network::MANAGEMENT_PURPOSE),
        };
        xapi::pif::set_other_config(s, pif, &config).await?;
    }
    Ok(())
}

struct Ctx<'a> {
    shared: &'a Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &'a Session,
    /// Hosts in the order that numbers a pool's address range.
    hosts: Vec<Ref<Host>>,
}

impl Ctx<'_> {
    fn model(&self) -> Arc<ConnectionModel> {
        model(self.shared, self.connection)
    }

    fn name(&self, pif: &Ref<Pif>) -> String {
        let m = self.model();
        m.get(pif)
            .map(|p| objects::pif_name(&m, p))
            .unwrap_or_default()
    }

    async fn task(
        &self,
        start: impl std::future::Future<Output = xapi::Result<Ref<xapi::Task>>>,
        goes_away: bool,
    ) -> Result<(), TaskOutcome> {
        let at = self
            .shared
            .state()
            .operations
            .iter()
            .find(|o| o.id == self.op)
            .and_then(|o| o.progress)
            .unwrap_or(0.0);
        done(run_task(self.shared, self.op, self.s, start, Span(at, at), goes_away).await)
    }

    /// `NetworkingActionHelpers.BringUp`.
    async fn bring_up(
        &self,
        config: &PifConfig,
        existing: &Ref<Pif>,
        ip: &str,
    ) -> Result<(), TaskOutcome> {
        let primary = config.purpose.is_none();
        step(
            self.shared,
            self.op,
            OperationStep::BringingUp {
                pif: self.name(existing),
            },
        );
        tri!(save_purpose(self.s, existing, !primary, config.purpose.as_deref()).await);
        self.reconfigure_ip(
            existing,
            &config.mode,
            ip,
            &config.netmask,
            &config.gateway,
            &config.dns,
        )
        .await?;
        if !primary && !tri!(xapi::pif::get_currently_attached(self.s, existing).await) {
            self.task(xapi::pif::async_plug(self.s, existing), false)
                .await?;
        }
        Ok(())
    }

    async fn reconfigure_ip(
        &self,
        pif: &Ref<Pif>,
        mode: &IpConfigurationMode,
        ip: &str,
        netmask: &str,
        gateway: &str,
        dns: &str,
    ) -> Result<(), TaskOutcome> {
        // The management interface's own address changing drops the link.
        let management = self.model().get(pif).is_some_and(|p| p.management);
        let start = xapi::pif::async_reconfigure_ip(self.s, pif, mode, ip, netmask, gateway, dns);
        self.task(start, management).await
    }

    /// `Depurpose`.
    async fn depurpose(&self, pif: &Ref<Pif>) -> Result<(), TaskOutcome> {
        step(
            self.shared,
            self.op,
            OperationStep::Depurposing {
                pif: self.name(pif),
            },
        );
        tri!(save_purpose(self.s, pif, false, None).await);
        Ok(())
    }

    /// `ClearIP`: not on an interface clustering uses.
    async fn clear_ip(&self, pif: &Ref<Pif>) -> Result<(), TaskOutcome> {
        if network::pif_used_by_clustering(&self.model(), pif) {
            return Ok(());
        }
        step(
            self.shared,
            self.op,
            OperationStep::BringingDown {
                pif: self.name(pif),
            },
        );
        self.reconfigure_ip(pif, &IpConfigurationMode::None, "", "", "", "")
            .await
    }

    /// `BringDown`: `Depurpose` then `ClearIP`.
    async fn bring_down(&self, pif: &Ref<Pif>) -> Result<(), TaskOutcome> {
        self.depurpose(pif).await?;
        self.clear_ip(pif).await
    }

    /// `ChangeNetworkingAction.Reconfigure` of the up interface `config`.
    async fn up(&self, config: &PifConfig, this_host: bool, pool: bool) -> Result<(), TaskOutcome> {
        let m = self.model();
        for (p, record) in some_hosts(&m, &config.pif, this_host) {
            let ip = if config.mode == IpConfigurationMode::Static {
                if pool {
                    let index = self
                        .hosts
                        .iter()
                        .position(|h| *h == record.host)
                        .ok_or_else(|| {
                            TaskOutcome::Failed(network::client_error(errors::NO_COORDINATOR, &[]))
                        })?;
                    network::ip_in_range(&config.ip, index).unwrap_or_else(|| config.ip.clone())
                } else {
                    config.ip.clone()
                }
            } else {
                record.ip.clone()
            };
            self.bring_up(config, &p, &ip).await?;
        }
        Ok(())
    }

    async fn down(&self, pif: &Ref<Pif>, this_host: bool) -> Result<(), TaskOutcome> {
        let m = self.model();
        for (p, _) in some_hosts(&m, pif, this_host) {
            self.bring_down(&p).await?;
        }
        Ok(())
    }

    /// `NetworkingActionHelpers.ReconfigureManagement` on one host.
    async fn reconfigure_management(
        &self,
        down: &Ref<Pif>,
        up: &Ref<Pif>,
        bring_down_down: bool,
    ) -> Result<(), TaskOutcome> {
        let m = self.model();
        if bring_down_down {
            for (p, _) in some_hosts(&m, down, true) {
                self.depurpose(&p).await?;
            }
        }
        for (p, _) in some_hosts(&m, up, true) {
            step(
                self.shared,
                self.op,
                OperationStep::ManagementReconfiguring { pif: self.name(&p) },
            );
            tri!(save_purpose(self.s, &p, false, None).await);
            self.task(xapi::host::async_management_reconfigure(self.s, &p), true)
                .await?;
        }
        if bring_down_down {
            for (p, _) in some_hosts(&self.model(), down, true) {
                self.clear_ip(&p).await?;
            }
        }
        Ok(())
    }

    /// `NetworkingActionHelpers.PoolReconfigureManagement`.
    async fn pool_reconfigure_management(
        &self,
        up: &Ref<Pif>,
        down: &Ref<Pif>,
    ) -> Result<(), TaskOutcome> {
        let m = self.model();
        let Some(network) = m.get(up).map(|p| p.network.clone()) else {
            return Err(TaskOutcome::Failed(network::client_error(
                errors::NETWORK_GONE,
                &[],
            )));
        };
        step(
            self.shared,
            self.op,
            OperationStep::ManagementReconfiguring { pif: self.name(up) },
        );
        self.task(
            xapi::pool::async_management_reconfigure(self.s, &network),
            true,
        )
        .await?;
        step(self.shared, self.op, OperationStep::WaitingForMembers);
        self.wait_for_members().await;
        for this_host in [false, true] {
            for (p, _) in some_hosts(&self.model(), down, this_host) {
                self.clear_ip(&p).await?;
            }
        }
        Ok(())
    }

    /// `WaitForMembersToRecover`: the other hosts go away, then come back
    /// (each phase bounded).
    async fn wait_for_members(&self) {
        let options = &self.shared.options;
        let tries = (options.member_recovery_timeout.as_millis()
            / options.member_recovery_poll.as_millis().max(1))
        .max(1);
        let members = |m: &ConnectionModel| -> Vec<Ref<Host>> {
            m.hosts
                .keys()
                .filter(|h| !objects::is_coordinator(m, h))
                .cloned()
                .collect()
        };
        let mut dead: Vec<Ref<Host>> = Vec::new();
        let mut attempt = 0;
        loop {
            let m = self.model();
            let all = members(&m);
            if dead.len() >= all.len() || attempt > tries {
                break;
            }
            for h in &all {
                let live = m.hosts.get(h).is_some_and(|r| objects::host_is_live(&m, r));
                if !live && !dead.contains(h) {
                    dead.push(h.clone());
                }
            }
            attempt += 1;
            tokio::time::sleep(options.member_recovery_poll).await;
        }
        attempt = 0;
        while !dead.is_empty() && attempt <= tries {
            let m = self.model();
            dead.retain(|h| !m.hosts.get(h).is_some_and(|r| objects::host_is_live(&m, r)));
            attempt += 1;
            tokio::time::sleep(options.member_recovery_poll).await;
        }
    }
}

async fn change_networking(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    change: &IpChange,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let ctx = Ctx {
        shared,
        connection,
        op,
        s,
        hosts: objects::sorted_hosts(&m)
            .into_iter()
            .map(|(r, _)| r.clone())
            .collect(),
    };
    let parts = (change.up.len()
        + change.down.len()
        + usize::from(change.down_management.is_some()))
    .max(1) as f64;
    let inc = if change.pool { 0.5 } else { 1.0 } / parts;
    let mut at = 0.0;
    let mut advance = |shared: &Shared| {
        at += inc;
        progress(shared, op, at);
    };
    // Up on the other hosts, then this one.
    if change.pool {
        for config in &change.up {
            ctx.up(config, false, true).await?;
            advance(shared);
        }
    }
    for config in &change.up {
        ctx.up(config, true, change.pool).await?;
        advance(shared);
    }
    if let Some(down_management) = &change.down_management {
        let Some(new_management) = &change.new_management else {
            return Ok(());
        };
        if change.pool {
            ctx.pool_reconfigure_management(&new_management.pif, down_management)
                .await?;
            // The original stops here (`PoolReconfigureManagement(); return;`).
            return Ok(());
        }
        let clear = !change.up.iter().any(|c| &c.pif == down_management);
        ctx.reconfigure_management(down_management, &new_management.pif, clear)
            .await?;
        advance(shared);
    }
    if change.pool {
        for pif in &change.down {
            ctx.down(pif, false).await?;
            advance(shared);
        }
    }
    for pif in &change.down {
        ctx.down(pif, true).await?;
        advance(shared);
    }
    Ok(())
}

/// `SetSecondaryManagementPurposeAction`.
async fn rename_interfaces(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    pool: bool,
    pifs: &[(Ref<Pif>, String)],
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    for (pif, purpose) in pifs {
        let Some(record) = m.get(pif) else {
            continue;
        };
        let Some(net) = m.networks.get(&record.network) else {
            tracing::warn!("network has gone away");
            return Ok(());
        };
        for (p, other) in network::network_pifs(&m, net) {
            if !pool && other.host != record.host {
                continue;
            }
            let mut config = tri!(xapi::pif::get_other_config(s, p).await);
            config.insert(network::MANAGEMENT_PURPOSE.to_owned(), purpose.clone());
            tri!(xapi::pif::set_other_config(s, p, &config).await);
        }
    }
    Ok(())
}

// ---- VIFs --------------------------------------------------------------------

fn vm_is_running(m: &ConnectionModel, vm: &Ref<Vm>) -> bool {
    m.vms
        .get(vm)
        .is_some_and(|v| v.power_state == VmPowerState::Running)
}

/// `CreateVIFAction`: the interface is made and, on a running VM,
/// hot-plugged when allowed (else the user is told to restart the VM).
#[allow(clippy::too_many_arguments)]
async fn create_vif(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vm: &Ref<Vm>,
    spec: &VifSpec,
    existing: Option<&VifRecord>,
    span: Span,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let record = network::vif_record(vm, spec, existing);
    let vif = made::<Vif>(shared, op, s, xapi::vif::async_create(s, &record), span).await?;
    if vm_is_running(&m, vm) {
        let allowed = tri!(xapi::vif::get_allowed_operations(s, &vif).await);
        if allowed.contains(&VifOperations::Plug) {
            tri!(xapi::vif::plug(s, &vif).await);
        } else {
            shared.update_operation(op, |o| o.notice = Some(OperationNotice::RestartForVif));
        }
    }
    Ok(())
}

/// `DeleteVIFAction`: unplugged first on a running VM when allowed
/// (already detached is fine), then destroyed.
async fn delete_vif(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vif: &Ref<Vif>,
    span: Span,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let vm = m.get(vif).map(|v| v.vm.clone()).unwrap_or_default();
    if vm_is_running(&m, &vm)
        && tri!(xapi::vif::get_allowed_operations(s, vif).await).contains(&VifOperations::Unplug)
        && let Err(e) = xapi::vif::unplug(s, vif).await
        && !e.is_api("DEVICE_ALREADY_DETACHED")
    {
        return Err(TaskOutcome::Failed(e));
    }
    done(run_task(shared, op, s, xapi::vif::async_destroy(s, vif), span, false).await)
}

/// `UpdateVIFAction`: the interface is deleted and made again with the new
/// settings (and its other fields).
async fn update_vif(
    shared: &Shared,
    connection: ConnectionId,
    op: OperationId,
    s: &Session,
    vif: &Ref<Vif>,
    spec: &VifSpec,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let Some(existing) = m.get(vif).map(|v| (**v).clone()) else {
        return Err(TaskOutcome::Failed(xapi::Error::Api {
            code: codes::HANDLE_INVALID.to_owned(),
            params: vec!["VIF".to_owned(), vif.to_string()],
        }));
    };
    delete_vif(shared, connection, op, s, vif, Span(0.0, 0.5)).await?;
    create_vif(
        shared,
        connection,
        op,
        s,
        &existing.vm,
        spec,
        Some(&existing),
        Span(0.5, 1.0),
    )
    .await
}

/// `PlugVIFAction` / `UnplugVIFAction`: only on a running VM, when the
/// server allows it.
async fn plug_vif(
    shared: &Shared,
    connection: ConnectionId,
    s: &Session,
    vif: &Ref<Vif>,
    plug: bool,
) -> Result<(), TaskOutcome> {
    let m = model(shared, connection);
    let vm = m.get(vif).map(|v| v.vm.clone()).unwrap_or_default();
    if !vm_is_running(&m, &vm) {
        return Ok(());
    }
    let wanted = if plug {
        VifOperations::Plug
    } else {
        VifOperations::Unplug
    };
    if tri!(xapi::vif::get_allowed_operations(s, vif).await).contains(&wanted) {
        if plug {
            tri!(xapi::vif::plug(s, vif).await);
        } else {
            tri!(xapi::vif::unplug(s, vif).await);
        }
    }
    Ok(())
}
