//! Ref hover highlight: hovering a graph pill for a moment
//! fades the commits it can't reach.

use egui::Ui;

/// Hovering a graph pill this long highlights its history.
const HOVER_SECS: f64 = 0.05;

pub fn hover_ref(ui: &Ui, resp: &egui::Response, target: &str) {
    if !resp.hovered() {
        return;
    }
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    let since = ui.data_mut(|m| {
        let e = m.get_temp_mut_or_insert_with(egui::Id::new("ref_hover"), || (String::new(), now, frame));
        if e.0 != target || e.2 + 1 < frame {
            *e = (target.to_owned(), now, frame);
        } else {
            e.2 = frame;
        }
        e.1
    });
    let wait = HOVER_SECS - (now - since);
    if wait > 0.0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }
}

/// The commit of the ref hovered long enough to highlight (pills report a frame late).
pub(super) fn hovered_ref(ui: &Ui) -> Option<String> {
    let (now, frame) = (ui.input(|i| i.time), ui.ctx().cumulative_frame_nr());
    let (tip, since, last) = ui.data(|m| m.get_temp::<(String, f64, u64)>(egui::Id::new("ref_hover")))?;
    (last + 1 >= frame && now - since >= HOVER_SECS).then_some(tip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, RawInput, Rect, pos2, vec2};
    use std::time::Duration;

    #[test]
    fn a_hovered_pill_asks_for_a_repaint_only_until_its_highlight_shows() {
        let ctx = egui::Context::default();
        let frame = |time: f64, events: Vec<Event>| {
            let raw = RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0))), time: Some(time), events, ..Default::default() };
            let mut shown = None;
            let mut out = ctx.run_ui(raw, |ui| {
                shown = hovered_ref(ui);
                let pill = ui.interact(Rect::from_min_size(pos2(0.0, 0.0), vec2(50.0, 50.0)), egui::Id::new("pill"), egui::Sense::click());
                hover_ref(ui, &pill, "tip");
            });
            out.textures_delta.clear();
            (out.viewport_output[&egui::ViewportId::ROOT].repaint_delay, shown)
        };
        frame(0.0, vec![]);
        frame(1.0, vec![Event::PointerMoved(pos2(20.0, 20.0))]);
        frame(1.01, vec![]);
        let (due, shown) = frame(1.02, vec![]);
        assert!(due <= Duration::from_secs_f64(HOVER_SECS) && shown.is_none(), "{due:?} {shown:?}");
        for time in [1.2, 1.3] {
            let (due, shown) = frame(time, vec![]);
            assert_eq!((due, shown.as_deref()), (Duration::MAX, Some("tip")), "at {time}");
        }
    }
}
