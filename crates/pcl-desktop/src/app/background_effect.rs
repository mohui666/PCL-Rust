//! Local image layout and cached blur. No source files are rewritten.
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

pub(super) const FITS: [(u8, &str); 13] = [
    (0, "自动"),
    (4, "平铺"),
    (1, "居中"),
    (3, "拉伸"),
    (5, "左上 (保持长宽比)"),
    (9, "左中 (保持长宽比)"),
    (7, "左下 (保持长宽比)"),
    (11, "中上 (保持长宽比)"),
    (2, "居中 (保持长宽比)"),
    (12, "中下 (保持长宽比)"),
    (6, "右上 (保持长宽比)"),
    (10, "右中 (保持长宽比)"),
    (8, "右下 (保持长宽比)"),
];
#[derive(Clone, Copy, Debug)]
pub(super) struct Placement {
    pub rect: Rect,
    pub uv: Rect,
    pub repeat: bool,
}
impl Placement {
    pub fn uv_at(self, point: Pos2) -> Pos2 {
        Pos2::new(
            self.uv.left() + (point.x - self.rect.left()) / self.rect.width() * self.uv.width(),
            self.uv.top() + (point.y - self.rect.top()) / self.rect.height() * self.uv.height(),
        )
    }
}
/// FormMain.UpdateBackgroundAndTitleBar: auto chooses tile only when both
/// dimensions are under half the body; all nine aligned modes are UniformToFill.
pub(super) fn placement(image: [usize; 2], area: Rect, fit: u8) -> Placement {
    let image = Vec2::new(image[0] as f32, image[1] as f32);
    let fit = resolved_fit([image.x as usize, image.y as usize], area.size(), fit);
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    match fit {
        1 => Placement {
            rect: Rect::from_center_size(area.center(), image),
            uv: full,
            repeat: false,
        },
        3 => Placement {
            rect: area,
            uv: full,
            repeat: false,
        },
        4 => Placement {
            rect: area,
            uv: Rect::from_min_max(
                Pos2::ZERO,
                Pos2::new(area.width() / image.x, area.height() / image.y),
            ),
            repeat: true,
        },
        _ => {
            let scale = (area.width() / image.x).max(area.height() / image.y);
            let visible = area.size() / (image * scale);
            let alignment = match fit {
                5 => Vec2::ZERO,
                6 => Vec2::new(1., 0.),
                7 => Vec2::new(0., 1.),
                8 => Vec2::splat(1.),
                9 => Vec2::new(0., 0.5),
                10 => Vec2::new(1., 0.5),
                11 => Vec2::new(0.5, 0.),
                12 => Vec2::new(0.5, 1.),
                _ => Vec2::splat(0.5),
            };
            let origin = (Vec2::splat(1.0) - visible) * alignment;
            Placement {
                rect: area,
                uv: Rect::from_min_size(Pos2::new(origin.x, origin.y), visible),
                repeat: false,
            }
        }
    }
}

pub(super) fn resolved_fit(image: [usize; 2], body: Vec2, fit: u8) -> u8 {
    if fit != 0 {
        fit
    } else if (image[0] as f32) < body.x / 2.0 && (image[1] as f32) < body.y / 2.0 {
        4
    } else {
        2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlurKey {
    pub source: egui::TextureId,
    pub body: [u32; 2],
    pub fit: u8,
    pub blur: u8,
}
impl BlurKey {
    pub fn margin(self) -> f32 {
        (f32::from(self.blur) + 1.0) / 1.8
    }
    pub fn area(self) -> Rect {
        Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(self.body[0] as f32, self.body[1] as f32),
        )
        .expand(self.margin())
    }
}
/// Premultiplied input avoids colored fringes at transparent borders. Render a
/// bounded canvas first, so a 1x4096 image never causes a giant cover resize.
/// WPF BlurEffect and image's fast Gaussian are different rasterizers: the
/// source's 0..40 control/radius geometry is retained, not a pixel-equivalence claim.
pub(super) fn blurred(source: &egui::ColorImage, key: BlurKey) -> egui::ColorImage {
    let area = key.area();
    let scale = (2048.0 / area.width().max(area.height())).min(1.0);
    let width = (area.width() * scale).ceil().max(1.0) as u32;
    let height = (area.height() * scale).ceil().max(1.0) as u32;
    let map = placement(
        source.size,
        area,
        resolved_fit(
            source.size,
            Vec2::new(key.body[0] as f32, key.body[1] as f32),
            key.fit,
        ),
    );
    let mut pixels = image::RgbaImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let point = area.min
            + Vec2::new(
                (x as f32 + 0.5) / width as f32 * area.width(),
                (y as f32 + 0.5) / height as f32 * area.height(),
            );
        *pixel = image::Rgba(sample(source, map, point).to_array());
    }
    let sigma = (f32::from(key.blur) + 1.0) / 3.0 * scale;
    let output = image::imageops::fast_blur(&pixels, sigma.max(0.01));
    egui::ColorImage::from_rgba_premultiplied([width as usize, height as usize], &output)
}
fn sample(source: &egui::ColorImage, map: Placement, point: Pos2) -> Color32 {
    if !map.rect.contains(point) {
        return Color32::TRANSPARENT;
    }
    let uv = map.uv_at(point);
    let x = uv.x * source.size[0] as f32 - 0.5;
    let y = uv.y * source.size[1] as f32 - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let pixel = |x: i32, y: i32| {
        let axis = |v: i32, n: usize| {
            if map.repeat {
                v.rem_euclid(n as i32) as usize
            } else {
                v.clamp(0, n as i32 - 1) as usize
            }
        };
        source.pixels[axis(y, source.size[1]) * source.size[0] + axis(x, source.size[0])].to_array()
    };
    let a = pixel(x0 as i32, y0 as i32);
    let b = pixel(x0 as i32 + 1, y0 as i32);
    let c = pixel(x0 as i32, y0 as i32 + 1);
    let d = pixel(x0 as i32 + 1, y0 as i32 + 1);
    let values: [u8; 4] = std::array::from_fn(|i| {
        egui::lerp(
            egui::lerp(a[i] as f32..=b[i] as f32, x - x0)
                ..=egui::lerp(c[i] as f32..=d[i] as f32, x - x0),
            y - y0,
        )
        .round() as u8
    });
    Color32::from_rgba_premultiplied(values[0], values[1], values[2], values[3])
}

/// Intersect the rounded body polygon with the real image bounds for Stretch.None.
pub(super) fn clip_polygon(mut points: Vec<Pos2>, rect: Rect) -> Vec<Pos2> {
    for edge in 0..4 {
        let old = std::mem::take(&mut points);
        if old.is_empty() {
            break;
        }
        let dist = |p: Pos2| match edge {
            0 => p.x - rect.left(),
            1 => rect.right() - p.x,
            2 => p.y - rect.top(),
            _ => rect.bottom() - p.y,
        };
        let mut a = *old.last().expect("nonempty");
        for b in old {
            let da = dist(a);
            let db = dist(b);
            if (da >= 0.) != (db >= 0.) {
                points.push(a.lerp(b, da / (da - db)));
            }
            if db >= 0. {
                points.push(b);
            }
            a = b;
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_modes_preserve_source_alignment_and_auto_threshold() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.));
        let left = placement([400, 200], area, 5);
        let middle = placement([400, 200], area, 2);
        let right = placement([400, 200], area, 6);
        assert_eq!(
            (left.uv.left(), middle.uv.left(), right.uv.left()),
            (0., 0.25, 0.5)
        );
        assert_eq!(
            placement([20, 30], area, 1).rect.size(),
            Vec2::new(20., 30.)
        );
        assert_eq!(placement([20, 30], area, 3).uv.size(), Vec2::splat(1.));
        assert!(placement([49, 49], area, 0).repeat);
        assert!(!placement([50, 49], area, 0).repeat);
        assert_eq!(FITS.len(), 13);
    }
    #[test]
    fn blur_spreads_edges_without_modifying_source_and_preserves_transparent_black() {
        let mut source = egui::ColorImage::filled([64, 64], Color32::TRANSPARENT);
        for y in 0..64 {
            for x in 0..32 {
                source.pixels[y * 64 + x] = Color32::WHITE;
            }
        }
        let original = source.clone();
        let key = BlurKey {
            source: egui::TextureId::Managed(1),
            body: [64, 64],
            fit: 3,
            blur: 12,
        };
        let result = blurred(&source, key);
        assert_eq!(source.pixels, original.pixels);
        assert!(result.pixels.iter().any(|p| p.a() > 0 && p.a() < 255));
        assert!(result
            .pixels
            .iter()
            .all(|p| p.r() <= p.a() && p.g() <= p.a() && p.b() <= p.a()));
        let result = blurred(
            &egui::ColorImage::filled([1, 4096], Color32::TRANSPARENT),
            BlurKey {
                body: [5000, 3000],
                ..key
            },
        );
        assert!(result.size[0] <= 2048 && result.size[1] <= 2048);
        assert!(result.pixels.iter().all(|p| *p == Color32::TRANSPARENT));
    }
    #[test]
    fn center_and_tile_do_not_fake_cover_or_stretch() {
        let source = egui::ColorImage::new([2, 1], vec![Color32::RED, Color32::BLUE]);
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(8., 4.));
        let centered = placement(source.size, area, 1);
        assert_eq!(
            sample(&source, centered, Pos2::new(0.5, 0.5)),
            Color32::TRANSPARENT
        );
        let tile = placement(source.size, area, 4);
        assert_eq!(sample(&source, tile, Pos2::new(0.5, 0.5)), Color32::RED);
        assert_eq!(sample(&source, tile, Pos2::new(2.5, 0.5)), Color32::RED);
        assert_eq!(sample(&source, tile, Pos2::new(1.5, 0.5)), Color32::BLUE);
    }
}
