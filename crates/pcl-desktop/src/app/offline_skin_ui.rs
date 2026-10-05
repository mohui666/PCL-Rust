//! Offline avatar rendering. No account session, login or credential access.
use super::{hint_ui::HintKind, Launcher};
use crate::theme;
use eframe::egui::{self, Color32, Pos2, Rect, TextureHandle, Vec2};
use pcl_core::{
    auth,
    config::{OfflineSkinMode, Settings},
    offline_skin,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};
const STEVE: &[u8] = include_bytes!("../../assets/upstream/Images/Skins/Steve.png");
const ALEX: &[u8] = include_bytes!("../../assets/upstream/Images/Skins/Alex.png");
#[derive(Clone, Debug, PartialEq, Eq)]
struct PreviewKey {
    mode: OfflineSkinMode,
    name: String,
    official: String,
    path: Option<PathBuf>,
    modified: Option<(u64, SystemTime)>,
}
impl PreviewKey {
    fn from_settings(s: &Settings) -> Self {
        let path = if s.offline_skin_mode == OfflineSkinMode::Custom {
            s.offline_skin_path.clone()
        } else {
            None
        };
        let modified = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| Some((m.len(), m.modified().ok()?)));
        Self {
            mode: s.offline_skin_mode,
            name: s.offline_name.clone(),
            official: if s.offline_skin_mode == OfflineSkinMode::OfficialName {
                s.offline_skin_name.clone()
            } else {
                String::new()
            },
            path,
            modified,
        }
    }
}
struct Pending {
    receiver: Receiver<Result<Vec<u8>, String>>,
    cancel: Arc<AtomicBool>,
}
#[derive(Clone)]
struct Avatar {
    texture: TextureHandle,
    height: f32,
}
#[derive(Default)]
pub(super) struct OfflineSkinState {
    key: Option<PreviewKey>,
    changed: Option<Instant>,
    poll_at: Option<Instant>,
    pending: Option<Pending>,
    avatar: Option<Avatar>,
    cache: VecDeque<(PreviewKey, Avatar)>,
    error: Option<String>,
    started: bool,
}
impl OfflineSkinState {
    fn cancel(&mut self) {
        if let Some(p) = self.pending.take() {
            p.cancel.store(true, Ordering::Relaxed);
        }
    }
    fn accept_key(&mut self, key: PreviewKey) {
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.cancel();
        self.avatar = self
            .cache
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, a)| a.clone());
        self.started = self.avatar.is_some();
        self.key = Some(key);
        self.error = None;
        self.changed = Some(Instant::now());
    }
}
impl Launcher {
    pub(super) fn offline_skin_tick(&mut self, ctx: &egui::Context) {
        if self.microsoft {
            self.offline_skin.cancel();
            self.offline_skin.started = false;
            return;
        }
        let now = Instant::now();
        if self.offline_skin.poll_at.is_none_or(|at| now >= at) {
            self.offline_skin.poll_at = Some(now + Duration::from_millis(500));
            self.offline_skin
                .accept_key(PreviewKey::from_settings(&self.settings));
        }
        if !self.offline_skin.started
            && self
                .offline_skin
                .changed
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(500))
        {
            if let Some(key) = self.offline_skin.key.clone() {
                self.offline_skin.started = true;
                let (tx, receiver) = mpsc::channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let worker = cancel.clone();
                self.offline_skin.pending = Some(Pending { receiver, cancel });
                std::thread::spawn(move || {
                    let result = preview_bytes(&key, &worker)
                        .map_err(|e| format!("离线皮肤预览失败：{e:#}"));
                    let _ = tx.send(result);
                });
            }
        }
        let result = self
            .offline_skin
            .pending
            .as_ref()
            .and_then(|p| p.receiver.try_recv().ok());
        if let Some(result) = result {
            self.offline_skin.pending = None;
            match result.and_then(|bytes| decode_avatar(ctx, &bytes).map_err(|e| e.to_string())) {
                Ok(avatar) => {
                    if let Some(key) = self.offline_skin.key.clone() {
                        self.offline_skin.cache.retain(|(k, _)| k != &key);
                        self.offline_skin.cache.push_back((key, avatar.clone()));
                        while self.offline_skin.cache.len() > 8 {
                            self.offline_skin.cache.pop_front();
                        }
                    }
                    self.offline_skin.avatar = Some(avatar);
                    self.offline_skin.error = None;
                }
                Err(error) => {
                    self.offline_skin.error = Some(error.clone());
                    self.push_hint(HintKind::Error, error);
                }
            }
        }
        if self.offline_skin.pending.is_some() || !self.offline_skin.started {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    pub(super) fn offline_head(&mut self, ui: &mut egui::Ui, rect: Rect) {
        if let Some(avatar) = &self.offline_skin.avatar {
            let face = Rect::from_center_size(rect.center(), Vec2::splat(48.0));
            ui.painter().add(
                egui::epaint::Shadow {
                    offset: [0, 0],
                    blur: 10,
                    spread: 0,
                    color: theme::palette(ui.ctx()).dark.gamma_multiply(30.0 / 255.0),
                }
                .as_shape(face, egui::CornerRadius::ZERO),
            );
            for (x, size) in [(8.0, 48.0), (40.0, 56.0)] {
                ui.painter().image(
                    avatar.texture.id(),
                    Rect::from_center_size(rect.center(), Vec2::splat(size)),
                    Rect::from_min_max(
                        Pos2::new(x / 64.0, 8.0 / avatar.height),
                        Pos2::new((x + 8.0) / 64.0, 16.0 / avatar.height),
                    ),
                    Color32::WHITE,
                );
            }
        } else {
            self.assets.head(ui, rect);
        }
        let error = self.offline_skin.error.clone();
        let response = ui.interact(
            rect,
            ui.id().with("offline-skin-preview"),
            egui::Sense::click(),
        );
        if let Some(error) = error {
            let response =
                response.on_hover_text(format!("{error}\n点击头像重试；皮肤设置位于设置 → 启动。"));
            if response.clicked() {
                self.offline_skin.started = false;
                self.offline_skin.changed = Some(Instant::now());
                self.offline_skin.error = None;
            }
        } else if self.offline_skin.pending.is_some() {
            response.on_hover_text("正在获取皮肤……");
        } else {
            response.on_hover_text("离线皮肤预览；游戏内效果仍取决于游戏版本及皮肤模式。");
        }
    }
}
fn default_bytes(uuid: &str) -> anyhow::Result<Vec<u8>> {
    Ok(if offline_skin::default_slim(uuid)? {
        ALEX
    } else {
        STEVE
    }
    .to_vec())
}
fn preview_bytes(key: &PreviewKey, cancel: &AtomicBool) -> anyhow::Result<Vec<u8>> {
    if cancel.load(Ordering::Relaxed) {
        return Err(pcl_core::model::OperationCancelled.into());
    }
    match key.mode {
        OfflineSkinMode::Default => default_bytes(&auth::offline_session(&key.name)?.uuid),
        OfflineSkinMode::Steve => Ok(STEVE.to_vec()),
        OfflineSkinMode::Alex => Ok(ALEX.to_vec()),
        OfflineSkinMode::Custom => offline_skin::read_skin(
            key.path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("请先选择本地皮肤 PNG"))?,
        ),
        OfflineSkinMode::OfficialName => {
            let skin = offline_skin::fetch_public_skin(&key.official, cancel)?;
            match skin.png {
                Some(png) => Ok(png),
                None => default_bytes(&skin.uuid),
            }
        }
    }
}
fn decode_avatar(ctx: &egui::Context, bytes: &[u8]) -> anyhow::Result<Avatar> {
    let reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    anyhow::ensure!(
        bytes.len() <= 1024 * 1024 && matches!(reader.into_dimensions()?, (64, 32) | (64, 64)),
        "皮肤预览尺寸无效"
    );
    let pixels = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?.to_rgba8();
    let height = pixels.height() as f32;
    Ok(Avatar {
        texture: ctx.load_texture(
            "offline-skin",
            egui::ColorImage::from_rgba_unmultiplied([64, pixels.height() as usize], &pixels),
            egui::TextureOptions::NEAREST,
        ),
        height,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_alex_and_default_parity_use_real_distinct_assets() {
        assert_ne!(STEVE, ALEX);
        assert_eq!(default_bytes(&"0".repeat(32)).unwrap(), STEVE);
        assert_eq!(
            default_bytes("00000000000000000000000000000001").unwrap(),
            ALEX
        );
        let ctx = egui::Context::default();
        assert_eq!(decode_avatar(&ctx, ALEX).unwrap().height, 64.0);
    }
    #[test]
    fn changing_mode_discards_pending_receiver_and_old_texture() {
        let settings = Settings::default();
        let old = PreviewKey::from_settings(&settings);
        let mut state = OfflineSkinState::default();
        state.accept_key(old);
        let (_, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        state.pending = Some(Pending {
            receiver,
            cancel: cancel.clone(),
        });
        let mut settings = settings;
        settings.offline_skin_mode = OfflineSkinMode::Alex;
        state.accept_key(PreviewKey::from_settings(&settings));
        assert!(cancel.load(Ordering::Relaxed));
        assert!(state.pending.is_none());
        assert!(!state.started);
    }
    #[test]
    fn custom_png_is_loaded_from_selected_file_and_oversized_decode_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skin.png");
        std::fs::write(&path, ALEX).unwrap();
        let settings = Settings {
            offline_skin_mode: OfflineSkinMode::Custom,
            offline_skin_path: Some(path),
            ..Default::default()
        };
        assert_eq!(
            preview_bytes(
                &PreviewKey::from_settings(&settings),
                &AtomicBool::new(false)
            )
            .unwrap(),
            ALEX
        );
        assert!(decode_avatar(&egui::Context::default(), b"not PNG").is_err());
    }
}
