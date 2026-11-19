//! Development-only screenshot capture via egui's viewport screenshot callback.
//!
//! Enabled by `KESTREL_SHOT=<path.ppm>`. Optional `KESTREL_SHOT_DELAY` (frames,
//! default 40) and `KESTREL_SHOT_EXIT=1`. Writes an uncompressed PPM.

use std::sync::atomic::{AtomicBool, Ordering};

static DONE: AtomicBool = AtomicBool::new(false);
static SHOT_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Debug hotkey capture: writes to KESTREL_SHOT, with a numeric suffix for repeated shots.
pub fn capture_now(ctx: &egui::Context) {
    let Ok(base) = std::env::var("KESTREL_SHOT") else {
        return;
    };
    let n = SHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = if n == 0 {
        base
    } else {
        match base.rfind('.') {
            Some(dot) => format!("{}-{n}{}", &base[..dot], &base[dot..]),
            None => format!("{base}-{n}"),
        }
    };
    let callback = egui::ScreenshotCallback::new(move |image| {
        write_ppm(&path, &image);
    });
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
}

pub fn maybe_capture(ctx: &egui::Context, frame_count: &mut u64) {
    if std::env::var("KESTREL_SHOT_MANUAL").is_ok() {
        return;
    }
    let Ok(path) = std::env::var("KESTREL_SHOT") else {
        return;
    };
    if DONE.load(Ordering::Relaxed) {
        if std::env::var("KESTREL_SHOT_EXIT").is_ok() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        return;
    }
    let delay: u64 = std::env::var("KESTREL_SHOT_DELAY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    *frame_count += 1;
    if *frame_count < delay {
        ctx.request_repaint();
        return;
    }
    let callback = egui::ScreenshotCallback::new(move |image| {
        write_ppm(&path, &image);
        DONE.store(true, Ordering::Relaxed);
    });
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
    ctx.request_repaint();
}

fn write_ppm(path: &str, image: &egui::ColorImage) {
    let [w, h] = image.size;
    let mut out = Vec::with_capacity(w * h * 3 + 32);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for px in &image.pixels {
        let [r, g, b, _] = px.to_srgba_unmultiplied();
        out.push(r);
        out.push(g);
        out.push(b);
    }
    if let Err(err) = std::fs::write(path, out) {
        eprintln!("kestrel: failed to write screenshot: {err}");
    }
}
