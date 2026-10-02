//! Draws the listening bubble: a Siri-style swirling orb in neon colours
//! derived from the configured accent, with a glowing neon ring.

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, Mask, Paint, PathBuilder, Pixmap, Point, RadialGradient, Shader,
    SpreadMode, Stroke, SweepGradient, Transform,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Hidden = 0,
    Listening = 1,
    Thinking = 2,
    Success = 3,
    Error = 4,
}

impl Mode {
    pub fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Listening,
            2 => Mode::Thinking,
            3 => Mode::Success,
            4 => Mode::Error,
            _ => Mode::Hidden,
        }
    }
}

pub struct Frame {
    /// Seconds since the bubble appeared.
    pub t: f32,
    /// Seconds since the current mode started.
    pub mode_t: f32,
    /// Smoothed loudness, 0–1 (mic while listening, voice while speaking).
    pub level: f32,
    pub mode: Mode,
    pub opacity: f32,
    pub scale: f32,
    pub accent: (f32, f32, f32),
    /// The voice is talking — pulse like speech.
    pub speaking: bool,
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::from_rgba(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), a.clamp(0.0, 1.0)).unwrap()
}

fn radial(c: Point, radius: f32, stops: Vec<GradientStop>) -> Shader<'static> {
    RadialGradient::new(c, 0.0, c, radius.max(0.5), stops, SpreadMode::Pad, Transform::identity())
        .unwrap_or(Shader::SolidColor(Color::TRANSPARENT))
}

fn mix(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// Rotate a colour's hue by `deg`, keeping it vivid (neon companions for the accent).
fn hue_shift((r, g, b): (f32, f32, f32), deg: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = if d < 1e-5 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    h = (h + deg).rem_euclid(360.0);
    // Full saturation, full value: neon.
    let x = 1.0 - ((h / 60.0) % 2.0 - 1.0).abs();
    match (h / 60.0) as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    }
}

/// Draw one frame centred in `pix`. `diameter` is the resting size in pixels;
/// the pixmap should be ~2.2× that to leave room for the glow.
pub fn render(pix: &mut Pixmap, f: &Frame, diameter: f32) {
    pix.fill(Color::TRANSPARENT);
    if f.opacity <= 0.001 || f.scale <= 0.001 {
        return;
    }
    let (w, h) = (pix.width() as f32, pix.height() as f32);
    let c = Point::from_xy(w / 2.0, h / 2.0);
    let o = f.opacity;

    let mut level = f.level.clamp(0.0, 1.0);
    if f.speaking && level < 0.02 {
        // Windows voices give us no level: fake a syllable-ish pulse.
        let s = ((f.t * 11.0).sin() * 0.5 + 0.5) * ((f.t * 3.7).sin() * 0.3 + 0.7);
        level = level.max(0.25 + 0.45 * s);
    }

    let base = match f.mode {
        Mode::Success => mix(f.accent, (1.0, 1.0, 1.0), 0.25),
        Mode::Error => mix(f.accent, (1.0, 0.15, 0.3), 0.85),
        _ => f.accent,
    };
    // Accent plus two hue-shifted neon companions for the swirl.
    let palette = [base, hue_shift(base, 110.0), hue_shift(base, -95.0), mix(base, (1.0, 1.0, 1.0), 0.6)];
    let (ar, ag, ab) = base;

    let breathe = match f.mode {
        Mode::Thinking => 0.04 * (f.t * 5.0).sin(),
        Mode::Listening => 0.015 * (f.t * 2.2).sin(),
        Mode::Success => 0.08 * (f.mode_t * 10.0).sin() * (1.0 - f.mode_t * 1.5).max(0.0),
        _ => 0.0,
    };
    let shake = if f.mode == Mode::Error {
        (f.mode_t * 42.0).sin() * diameter * 0.05 * (1.0 - f.mode_t * 1.8).max(0.0)
    } else {
        0.0
    };
    let c = Point::from_xy(c.x + shake, c.y);
    let r = diameter / 2.0 * f.scale * (1.0 + 0.16 * level + breathe);

    let mut paint = Paint::default();
    paint.anti_alias = true;

    // 1. Neon glow.
    let glow_r = r * (1.8 + 0.35 * level);
    paint.shader = radial(
        c,
        glow_r,
        vec![
            GradientStop::new(0.0, rgba(ar, ag, ab, 0.5 * o)),
            GradientStop::new(r / glow_r, rgba(ar, ag, ab, (0.30 + 0.35 * level) * o)),
            GradientStop::new(1.0, rgba(ar, ag, ab, 0.0)),
        ],
    );
    if let Some(p) = PathBuilder::from_circle(c.x, c.y, glow_r) {
        pix.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }

    let Some(disc) = PathBuilder::from_circle(c.x, c.y, r) else { return };

    // 2. Near-black glass body.
    paint.shader = radial(
        c,
        r,
        vec![
            GradientStop::new(0.0, rgba(0.05, 0.05, 0.08, 0.95 * o)),
            GradientStop::new(1.0, rgba(0.02, 0.02, 0.04, 0.95 * o)),
        ],
    );
    pix.fill_path(&disc, &paint, FillRule::Winding, Transform::identity(), None);

    // 3. Swirling neon blobs, clipped to the disc.
    let mut mask = Mask::new(pix.width(), pix.height()).unwrap();
    mask.fill_path(&disc, FillRule::Winding, true, Transform::identity());
    let speed = match f.mode {
        Mode::Thinking => 3.4,
        Mode::Listening => 1.0 + 1.6 * level,
        _ => 0.8 + level,
    };
    let blobs = [(1.0f32, 0.0f32), (-0.8, 2.1), (0.65, 4.2), (-1.25, 1.0)];
    paint.blend_mode = BlendMode::Plus;
    for (i, ((spd, phase), col)) in blobs.iter().zip(palette.iter()).enumerate() {
        let ang = f.t * speed * spd + phase;
        let wobble = 1.0 + 0.25 * (f.t * 1.7 + i as f32).sin();
        let dist = r * (0.28 + 0.22 * level) * wobble;
        let bc = Point::from_xy(c.x + ang.cos() * dist, c.y + ang.sin() * dist);
        let br = r * if i == 3 { 0.42 } else { 0.62 + 0.3 * level };
        let a = if i == 3 { 0.6 } else { 0.85 };
        paint.shader = radial(
            bc,
            br,
            vec![
                GradientStop::new(0.0, rgba(col.0, col.1, col.2, a * o)),
                GradientStop::new(0.55, rgba(col.0, col.1, col.2, a * 0.45 * o)),
                GradientStop::new(1.0, rgba(col.0, col.1, col.2, 0.0)),
            ],
        );
        if let Some(p) = PathBuilder::from_circle(bc.x, bc.y, br) {
            pix.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), Some(&mask));
        }
    }
    paint.blend_mode = BlendMode::SourceOver;

    // 4. Glassy highlight, top-left.
    let hc = Point::from_xy(c.x - r * 0.32, c.y - r * 0.38);
    paint.shader = radial(
        hc,
        r * 0.55,
        vec![
            GradientStop::new(0.0, rgba(1.0, 1.0, 1.0, 0.28 * o)),
            GradientStop::new(1.0, rgba(1.0, 1.0, 1.0, 0.0)),
        ],
    );
    pix.fill_path(&disc, &paint, FillRule::Winding, Transform::identity(), Some(&mask));

    // 5. Neon ring: a spinning comet while thinking, a glowing tube otherwise.
    let ring_w = (r * 0.06).max(1.5);
    if f.mode == Mode::Thinking {
        let rot = (f.t * 360.0 * 1.3) % 360.0;
        paint.shader = SweepGradient::new(
            c,
            0.0,
            360.0,
            vec![
                GradientStop::new(0.0, rgba(ar, ag, ab, 0.05 * o)),
                GradientStop::new(0.7, rgba(ar, ag, ab, 0.6 * o)),
                GradientStop::new(1.0, rgba(1.0, 1.0, 1.0, o)),
            ],
            SpreadMode::Pad,
            Transform::from_rotate_at(rot, c.x, c.y),
        )
        .unwrap_or(Shader::SolidColor(rgba(ar, ag, ab, o)));
        pix.stroke_path(&disc, &paint, &Stroke { width: ring_w * 1.6, ..Default::default() }, Transform::identity(), None);
    } else {
        let a = 0.6 + 0.4 * level;
        for (wm, k) in [(4.5, 0.10), (2.5, 0.22)] {
            paint.shader = Shader::SolidColor(rgba(ar, ag, ab, k * a * o));
            pix.stroke_path(&disc, &paint, &Stroke { width: ring_w * wm, ..Default::default() }, Transform::identity(), None);
        }
        paint.shader = Shader::SolidColor(rgba(ar * 0.6 + 0.4, ag * 0.6 + 0.4, ab * 0.6 + 0.4, a * o));
        pix.stroke_path(&disc, &paint, &Stroke { width: ring_w, ..Default::default() }, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes preview frames to target/bubble-*.png for eyeballing.
    #[test]
    fn preview_frames() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        for (name, accent, mode, level) in [
            ("lime-listening", (0.714, 1.0, 0.18), Mode::Listening, 0.6),
            ("lime-thinking", (0.714, 1.0, 0.18), Mode::Thinking, 0.0),
            ("magenta-listening", (1.0, 0.17, 0.84), Mode::Listening, 0.4),
            ("cyan-listening", (0.13, 0.95, 1.0), Mode::Listening, 0.4),
            ("lime-error", (0.714, 1.0, 0.18), Mode::Error, 0.0),
        ] {
            let mut pix = Pixmap::new(240, 240).unwrap();
            let f = Frame { t: 1.3, mode_t: 0.4, level, mode, opacity: 1.0, scale: 1.0, accent, speaking: false };
            render(&mut pix, &f, 100.0);
            pix.save_png(out.join(format!("bubble-{name}.png"))).unwrap();
        }
    }
}

#[cfg(test)]
mod icon {
    use super::*;

    /// Regenerates assets/icon.ico (PNG-compressed entries, Vista+ format).
    #[test]
    #[ignore]
    fn write_icon() {
        let sizes = [16u32, 24, 32, 48, 64, 128, 256];
        let mut images = Vec::new();
        for &s in &sizes {
            let mut pix = Pixmap::new(s, s).unwrap();
            let f = Frame { t: 1.1, mode_t: 0.0, level: 0.35, mode: Mode::Listening, opacity: 1.0, scale: 1.0, accent: (0.714, 1.0, 0.18), speaking: false };
            // Diameter chosen so the ring + a little glow fit the square.
            render(&mut pix, &f, s as f32 * 0.66);
            images.push(pix.encode_png().unwrap());
        }
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (s, png) in sizes.iter().zip(&images) {
            let b = if *s >= 256 { 0 } else { *s as u8 };
            ico.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += png.len() as u32;
        }
        for png in &images {
            ico.extend_from_slice(png);
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("icon.ico"), ico).unwrap();
        std::fs::write(dir.join("icon-256.png"), images.last().unwrap()).unwrap();
    }
}
