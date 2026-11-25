//! Drawing of General-tab sections and list tables.

use egui::{CollapsingHeader, Grid, RichText, ScrollArea, Ui};

use super::{Section, Table};
use crate::tab_strings::tab_strings;

/// Draws General-tab sections as collapsible groups. Like the original,
/// only "General" starts expanded; the expanded set is remembered per
/// object kind (`kind`), not per object.
pub fn sections(ui: &mut Ui, kind: &str, sections: &[Section]) {
    let general = tab_strings().sec_general;
    for section in sections {
        CollapsingHeader::new(RichText::new(section.title).strong())
            .id_salt(("general-section", kind, section.title))
            .default_open(section.title == general)
            .show(ui, |ui| {
                Grid::new(("general-rows", kind, section.title))
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .min_col_width(140.0)
                    .show(ui, |ui| {
                        for row in &section.rows {
                            ui.label(RichText::new(&row.label).weak());
                            if row.warn {
                                ui.colored_label(ui.visuals().error_fg_color, &row.value);
                            } else {
                                ui.label(&row.value);
                            }
                            ui.end_row();
                        }
                    });
            });
        ui.add_space(4.0);
    }
}

/// Draws a list table with a header row; scrolls sideways when wide.
pub fn table(ui: &mut Ui, id: &str, table: &Table) {
    if table.rows.is_empty() {
        ui.label(RichText::new(tab_strings().empty_list).weak());
        return;
    }
    ScrollArea::horizontal()
        .id_salt(("table-scroll", id))
        .show(ui, |ui| {
            Grid::new(("table", id))
                .num_columns(table.columns.len())
                .striped(true)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    for column in &table.columns {
                        ui.strong(*column);
                    }
                    ui.end_row();
                    for row in &table.rows {
                        for cell in &row.cells {
                            if row.greyed {
                                ui.label(RichText::new(cell).weak());
                            } else {
                                ui.label(cell);
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}
