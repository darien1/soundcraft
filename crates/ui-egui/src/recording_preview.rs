//! Draw the pending take as a passive overlay; stopping recording creates the real clips.

use crate::SoundApp;
use crate::edit_window::{sample_at, x_of};
use crate::recording_data::{first_input_channel, preview_span};
use crate::theme::regular;
use egui::{Align2, Color32, Painter, Rect, Shape, Stroke, StrokeKind, pos2};
use soundcraft_model::{Route, Session, Track, TrackKind};

pub(crate) fn draw(app: &SoundApp, painter: &Painter, session: &Session, track: &Track, timeline: Rect, lane: Rect) {
    if !app.engine.transport.recording || !track.mixer.record_arm || track.inactive || track.kind != TrackKind::Audio {
        return;
    }
    let preview = &app.recording_waveform;
    let selection = session.edit.selection;
    let loop_range = (session.edit.loop_record && !selection.is_empty()).then_some((selection.start, selection.end));
    let punch = app.punch.map(|range| (range.start, range.end));
    let Some(span) = preview_span(app.record_start, preview.frames, preview.rate, session.sample_rate.as_f64(), punch, loop_range) else {
        return;
    };
    let left = x_of(session, timeline, span.start);
    let right = x_of(session, timeline, span.end).max(left + 1.0);
    let rect = Rect::from_min_max(pos2(left, lane.min.y + 2.0), pos2(right, lane.max.y - 2.0));
    let visible = rect.intersect(lane).intersect(painter.clip_rect());
    if visible.width() <= 0.0 || visible.height() <= 2.0 {
        return;
    }
    let painter = painter.with_clip_rect(visible);
    let edge = Color32::from_rgb(238, 85, 91);
    let wave = Color32::from_rgb(255, 220, 222);
    painter.rect(rect, 1.0, Color32::from_rgba_unmultiplied(155, 32, 45, 175), Stroke::new(1.0, edge), StrokeKind::Inside);
    let name_h = if lane.height() >= 40.0 { 14.0 } else { 0.0 };
    if name_h > 0.0 && visible.width() > 35.0 {
        painter.text(pos2(visible.min.x + 4.0, rect.min.y + 7.0), Align2::LEFT_CENTER, "REC", regular(10.0), Color32::WHITE);
    }
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + name_h), rect.max);
    let first = match &track.mixer.input {
        Route::Hardware(route) => first_input_channel(route),
        _ => 0,
    };
    let nch = track.channels().max(1);
    let channel_h = body.height() / nch as f32;
    let zoom = session.edit.zoom.waveform_zoom;
    let zoom = if zoom.is_finite() { zoom.max(0.0) } else { 1.0 };
    let rectified = session.edit.flag("waveform.rectified");
    let power = session.edit.flag("waveform.power");
    let outlines = session.edit.flag("waveform.outlines");
    // The cap also bounds mesh allocation for pathological viewport dimensions.
    let columns = (visible.width().ceil() as usize).min(16_384);
    for channel in 0..nch {
        let index = first.saturating_add(channel) % preview.channels.len().max(1);
        let Some(envelope) = preview.channels.get(index) else { continue };
        let cy = body.min.y + channel_h * (channel as f32 + 0.5);
        let half = channel_h * 0.44;
        if half <= 0.0 {
            continue;
        }
        let mut mesh = egui::Mesh::default();
        for column in 0..columns {
            let x = visible.min.x + column as f32;
            let a = span.input_at(sample_at(session, timeline, x));
            let b = span.input_at(sample_at(session, timeline, x + 1.0));
            let (min, max) = envelope.minmax(a.max(0.0).floor() as usize, b.max(a + 1.0).ceil() as usize);
            let (mut top, mut bottom) = if rectified {
                let amplitude = (max.abs().max(min.abs()) * zoom).clamp(0.0, 1.0);
                (cy + half - amplitude * half * 2.0, cy + half)
            } else {
                (cy - (max * zoom).clamp(-1.0, 1.0) * half, cy - (min * zoom).clamp(-1.0, 1.0) * half)
            };
            if power {
                let compress = |y: f32| cy + (y - cy).signum() * ((y - cy).abs() / half).sqrt() * half;
                top = compress(top);
                bottom = compress(bottom);
            }
            if outlines {
                bottom = top + 1.0;
            }
            bottom = bottom.max(top + 1.0);
            let index = mesh.vertices.len() as u32;
            for point in [pos2(x, top), pos2(x + 1.0, top), pos2(x + 1.0, bottom), pos2(x, bottom)] {
                mesh.colored_vertex(point, wave);
            }
            mesh.add_triangle(index, index + 1, index + 2);
            mesh.add_triangle(index, index + 2, index + 3);
        }
        painter.with_clip_rect(body.intersect(visible)).add(Shape::mesh(mesh));
    }
    painter.line_segment([pos2(right - 0.5, rect.min.y), pos2(right - 0.5, rect.max.y)], Stroke::new(1.5, edge));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_waveform_renders_without_creating_session_clips() {
        let mut engine = soundcraft_engine::Engine::default();
        engine.execute("track.new", &serde_json::json!({"count": 1, "format": "stereo"})).unwrap();
        let id = engine.session().tracks.first().unwrap().id;
        engine.session_mut().track_mut(id).unwrap().mixer.record_arm = true;
        let mut app = SoundApp::new(engine, None, crate::Services::default());
        app.recorder_failed = true;
        app.engine.transport.recording = true;
        let data: Vec<f32> = (0..48_000 * 3)
            .flat_map(|i| {
                let signal = (i as f32 * 0.04).sin() * (0.2 + 0.6 * (i as f32 * 0.0001).sin().abs());
                [signal, signal * 0.5]
            })
            .collect();
        app.recording_waveform.append(&data, 2, 48_000);
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 700.0)).wgpu().build_ui_state(
            |ui, app: &mut SoundApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        for _ in 0..6 {
            harness.step();
        }
        assert_eq!(harness.state().recording_waveform.frames, 144_000);
        assert!(harness.state().engine.session().track(id).unwrap().clips().is_empty());
        let image = harness.render().unwrap();
        let red_pixels = image.pixels().filter(|pixel| pixel.0[0] > 90 && pixel.0[0] > pixel.0[1].saturating_add(40)).count();
        assert!(red_pixels > 1000, "the growing recording region must be visible");
        if let Ok(path) = std::env::var("SOUNDCRAFT_PREVIEW_SHOT") {
            image.save(path).unwrap();
        }
    }
}
