//! Ref hover highlight: hovering a ref (sidebar row or graph pill) for a moment
//! fades the commits it can't reach.

use egui::Ui;

/// Hovering a ref (sidebar row or graph pill) this long highlights its history.
const HOVER_SECS: f64 = 0.05;

pub fn hover_ref(ui: &Ui, resp: &egui::Response, target: &str) {
    if !resp.hovered() {
        return;
    }
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    ui.data_mut(|m| {
        let e = m.get_temp_mut_or_insert_with(egui::Id::new("ref_hover"), || (String::new(), now, frame));
        if e.0 != target || e.2 + 1 < frame {
            *e = (target.to_owned(), now, frame);
        } else {
            e.2 = frame;
        }
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(HOVER_SECS));
}

/// The commit of the ref hovered long enough to highlight (pills report a frame late).
pub(super) fn hovered_ref(ui: &Ui) -> Option<String> {
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    let (tip, since, last) = ui.data(|m| m.get_temp::<(String, f64, u64)>(egui::Id::new("ref_hover")))?;
    (last + 1 >= frame && now - since >= HOVER_SECS).then_some(tip)
}
