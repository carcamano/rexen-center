//! The Events panel: this client's operations (with progress and cancel)
//! and the tasks other clients run on the connected servers.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use egui::{Color32, Grid, ProgressBar, RichText, ScrollArea, Ui};
use xapi::{Ref, Task, TaskAllowedOperations, TaskRecord, TaskStatusType};
use xen_core::{AppState, BackendHandle, Command, ConnectionView, Operation, OperationStatus};

use crate::i18n::{self, fill, strings};
use crate::ops_strings::ops_strings;

const GREEN: Color32 = Color32::from_rgb(0x2e, 0xa0, 0x43);

/// Width of the progress column, kept even when no bar is shown so the
/// columns after it do not jump when a bar appears.
const PROGRESS_WIDTH: f32 = 120.0;

fn no_progress(ui: &mut Ui) {
    ui.allocate_space(egui::vec2(PROGRESS_WIDTH, 0.0));
}

/// Draws the panel. Returns whether something is still moving (so the
/// caller keeps repainting the relative times and progress).
pub fn show(ui: &mut Ui, state: &AppState, backend: &BackendHandle) -> bool {
    let s = strings();
    let ours: HashSet<&Ref<Task>> = state
        .operations
        .iter()
        .filter_map(|o| o.task.as_ref())
        .collect();
    let mut tasks: Vec<(&ConnectionView, &Ref<Task>, &TaskRecord)> = state
        .connections
        .iter()
        .flat_map(|c| c.model.tasks.iter().map(move |(r, t)| (&**c, r, &**t)))
        .filter(|(_, r, _)| !ours.contains(r))
        .collect();
    tasks.sort_by(|a, b| {
        b.2.created
            .unix_seconds()
            .cmp(&a.2.created.unix_seconds())
            .then_with(|| a.1.cmp(b.1))
    });

    // The title is the notifications area's tab (see `XenAdminApp`).
    ui.horizontal(|ui| {
        let finished = state.operations.iter().any(|o| !o.is_running());
        if ui
            .add_enabled(finished, egui::Button::new(s.events_clear))
            .clicked()
        {
            backend.send(Command::ClearFinishedOperations);
        }
    });
    ui.separator();

    let mut moving = false;
    ScrollArea::vertical()
        .auto_shrink(false)
        .id_salt("events-scroll")
        .show(ui, |ui| {
            if state.operations.is_empty() && tasks.is_empty() {
                ui.label(RichText::new(s.events_empty).weak());
                return;
            }
            Grid::new("events-grid")
                .num_columns(6)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    for title in [
                        s.events_col_status,
                        s.events_col_description,
                        s.events_col_server,
                        s.events_col_progress,
                        s.events_col_time,
                        "",
                    ] {
                        ui.label(RichText::new(title).strong());
                    }
                    ui.end_row();
                    for op in state.operations.iter().rev() {
                        let conn = op
                            .connection
                            .and_then(|c| state.connection(c))
                            .map(|c| &**c);
                        moving |= operation_row(ui, conn, op, backend);
                    }
                    if !tasks.is_empty() {
                        ui.label("");
                        ui.label(RichText::new(s.events_server_tasks).strong());
                        ui.end_row();
                    }
                    for (conn, task_ref, task) in tasks {
                        moving |= task_row(ui, conn, task_ref, task, backend);
                    }
                });
        });
    moving
}

fn ago(t: SystemTime) -> Duration {
    SystemTime::now().duration_since(t).unwrap_or_default()
}

fn operation_row(
    ui: &mut Ui,
    conn: Option<&ConnectionView>,
    op: &Operation,
    backend: &BackendHandle,
) -> bool {
    let s = strings();
    let label = i18n::operation_status_label(&op.status);
    match &op.status {
        OperationStatus::Running => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(label);
            });
        }
        OperationStatus::Succeeded => {
            ui.colored_label(GREEN, label);
        }
        OperationStatus::Cancelled => {
            ui.label(RichText::new(label).weak());
        }
        OperationStatus::Failed(_) => {
            ui.colored_label(ui.visuals().error_fg_color, label);
        }
    }
    let mut title = i18n::operation_title(&op.kind, &op.server);
    if let Some(user) = &op.elevated_as {
        title = format!(
            "{title} ({})",
            fill(ops_strings().op_elevated, &[("user", user)])
        );
    }
    match &op.status {
        OperationStatus::Failed(failure) => {
            ui.vertical(|ui| {
                ui.label(title);
                ui.label(
                    RichText::new(i18n::describe_operation_failure(conn, failure))
                        .small()
                        .color(ui.visuals().error_fg_color),
                );
            });
        }
        OperationStatus::Succeeded if op.notice.is_some() => {
            ui.vertical(|ui| {
                ui.label(title);
                if let Some(notice) = op.notice {
                    ui.label(
                        RichText::new(i18n::operation_notice(notice))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                    );
                }
            });
        }
        OperationStatus::Running if op.step.is_some() => {
            ui.vertical(|ui| {
                ui.label(title);
                if let Some(step) = &op.step {
                    ui.label(RichText::new(i18n::operation_step(step)).small().weak());
                }
            });
        }
        _ => {
            ui.label(title);
        }
    }
    ui.label(&op.server);
    match (op.is_running(), op.progress) {
        (true, Some(p)) => {
            ui.add(
                ProgressBar::new(p as f32)
                    .desired_width(PROGRESS_WIDTH)
                    .show_percentage(),
            );
        }
        _ => no_progress(ui),
    }
    let mut time = fill(
        s.time_ago,
        &[("time", &i18n::short_duration(ago(op.started)))],
    );
    if let Some(finished) = op.finished {
        let took = finished.duration_since(op.started).unwrap_or_default();
        time = format!(
            "{time} · {}",
            fill(s.time_took, &[("time", &i18n::short_duration(took))])
        );
    }
    ui.label(RichText::new(time).weak());
    if op.can_cancel() {
        if ui.button(s.events_cancel).clicked() {
            backend.send(Command::CancelOperation { operation: op.id });
        }
    } else {
        ui.label("");
    }
    ui.end_row();
    op.is_running()
}

fn task_row(
    ui: &mut Ui,
    conn: &ConnectionView,
    task_ref: &Ref<Task>,
    task: &TaskRecord,
    backend: &BackendHandle,
) -> bool {
    let s = strings();
    let pending = task.status == TaskStatusType::Pending;
    let label = i18n::task_status_label(&task.status);
    match &task.status {
        TaskStatusType::Pending | TaskStatusType::Cancelling => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(label);
            });
        }
        TaskStatusType::Success => {
            ui.colored_label(GREEN, label);
        }
        TaskStatusType::Failure => {
            ui.colored_label(ui.visuals().error_fg_color, label);
        }
        _ => {
            ui.label(RichText::new(label).weak());
        }
    }
    if task.error_info.is_empty() {
        ui.label(&task.name_label);
    } else {
        ui.vertical(|ui| {
            ui.label(&task.name_label);
            let mut info = task.error_info.iter().cloned();
            let error = xapi::Error::Api {
                code: info.next().unwrap_or_default(),
                params: info.collect(),
            };
            ui.label(
                RichText::new(i18n::describe_xapi_error_in(Some(conn), &error))
                    .small()
                    .color(ui.visuals().error_fg_color),
            );
        });
    }
    ui.label(&conn.server);
    if pending {
        ui.add(
            ProgressBar::new(task.progress.clamp(0.0, 1.0) as f32)
                .desired_width(PROGRESS_WIDTH)
                .show_percentage(),
        );
    } else {
        no_progress(ui);
    }
    let created = task
        .created
        .unix_seconds()
        .and_then(|secs| u64::try_from(secs).ok())
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
    match created {
        Some(t) => {
            ui.label(
                RichText::new(fill(s.time_ago, &[("time", &i18n::short_duration(ago(t)))])).weak(),
            );
        }
        None => {
            ui.label("");
        }
    }
    let can_cancel = pending
        && conn.status.is_connected()
        && task
            .allowed_operations
            .contains(&TaskAllowedOperations::Cancel);
    if can_cancel {
        if ui.button(s.events_cancel).clicked() {
            backend.send(Command::CancelTask {
                connection: conn.id,
                task: task_ref.clone(),
            });
        }
    } else {
        ui.label("");
    }
    ui.end_row();
    pending
}
