//! Local music only. No process command line, shell, or remote media is involved.
use anyhow::{bail, Context, Result};
use pcl_core::config::Settings;
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) trait Audio {
    fn open(path: &Path) -> Result<Self>
    where
        Self: Sized;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    fn volume(&mut self, volume: u16) -> Result<()>;
    fn position(&self) -> Result<(Duration, Duration, bool)>;
}

pub(super) type MusicState = PlayerState<native::Player>;

pub(super) struct PlayerState<P: Audio> {
    initialized: bool,
    files: Vec<PathBuf>,
    waiting: VecDeque<PathBuf>,
    current: Option<PathBuf>,
    player: Option<P>,
    playing: bool,
    volume: u16,
    pub(super) progress: f32,
    random_state: u64,
}

impl<P: Audio> Default for PlayerState<P> {
    fn default() -> Self {
        Self {
            initialized: false,
            files: Vec::new(),
            waiting: VecDeque::new(),
            current: None,
            player: None,
            playing: false,
            volume: 500,
            progress: 0.0,
            random_state: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                | 1,
        }
    }
}

impl<P: Audio> PlayerState<P> {
    pub(super) fn visible(&self) -> bool {
        !self.files.is_empty()
    }
    pub(super) fn playing(&self) -> bool {
        self.playing
    }
    pub(super) fn title(&self) -> String {
        self.current
            .as_deref()
            .and_then(Path::file_stem)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "背景音乐".into())
    }
    pub(super) fn tick(&mut self, folder: &Path, settings: &Settings) -> Result<()> {
        if !self.initialized {
            self.initialized = true;
            self.refresh(folder, settings, settings.ui_music_auto)?;
        }
        if self.volume != settings.ui_music_volume {
            if let Some(player) = &mut self.player {
                if let Err(error) = player.volume(settings.ui_music_volume) {
                    self.player = None;
                    self.playing = false;
                    return Err(error).context("调整音乐音量失败，播放已停止；可刷新音乐后重试");
                }
            }
            self.volume = settings.ui_music_volume;
        }
        if let Some(player) = &self.player {
            let (position, length, playing) = match player.position() {
                Ok(state) => state,
                Err(error) => {
                    self.player = None;
                    self.playing = false;
                    return Err(error)
                        .context("读取音乐播放状态失败，播放已停止；可刷新音乐后重试");
                }
            };
            self.progress = if length.is_zero() {
                0.0
            } else {
                (position.as_secs_f64() / length.as_secs_f64()).clamp(0.0, 1.0) as f32
            };
            // Paused/opened media is not an EOF. A playback request must have succeeded.
            if self.playing && !playing {
                self.next(settings, true)?;
            }
        }
        Ok(())
    }
    pub(super) fn refresh(
        &mut self,
        folder: &Path,
        settings: &Settings,
        start: bool,
    ) -> Result<()> {
        let files = collect_music(folder)?;
        self.player = None;
        self.current = None;
        self.playing = false;
        self.progress = 0.0;
        self.files = files;
        self.waiting.clear();
        self.next(settings, start)
    }
    fn refill(&mut self, random: bool) {
        let mut order = self.files.clone();
        if random {
            for end in (1..order.len()).rev() {
                self.random_state ^= self.random_state << 13;
                self.random_state ^= self.random_state >> 7;
                self.random_state ^= self.random_state << 17;
                order.swap(end, self.random_state as usize % (end + 1));
            }
        }
        if order.len() > 1 && order.first() == self.current.as_ref() {
            order.swap(0, 1);
        }
        self.waiting = order.into();
    }
    pub(super) fn next(&mut self, settings: &Settings, start: bool) -> Result<()> {
        self.player = None;
        self.playing = false;
        self.progress = 0.0;
        if self.waiting.is_empty() {
            self.refill(settings.ui_music_random);
        }
        let mut errors = Vec::new();
        while let Some(path) = self.waiting.pop_front() {
            let result = (|| {
                let meta = fs::symlink_metadata(&path).context("音乐文件不可访问")?;
                if !meta.is_file() || meta.file_type().is_symlink() {
                    bail!("音乐必须是普通本地文件");
                }
                let mut player = P::open(&path)?;
                player.volume(settings.ui_music_volume)?;
                if start {
                    player.play()?;
                }
                Ok(player)
            })();
            match result {
                Ok(player) => {
                    self.player = Some(player);
                    self.current = Some(path);
                    self.playing = start;
                    self.volume = settings.ui_music_volume;
                    break;
                }
                Err(error) => {
                    self.files.retain(|file| *file != path);
                    errors.push(format!(
                        "{}：{error:#}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
        if self.player.is_none() {
            self.current = None;
        }
        if !errors.is_empty() {
            bail!(
                "以下背景音乐无法播放，已跳过（刷新可重试）：\n{}",
                errors.join("\n")
            );
        }
        Ok(())
    }
    pub(super) fn set_playing(&mut self, playing: bool) -> Result<()> {
        if let Some(player) = &mut self.player {
            if playing {
                player.play()?;
            } else {
                player.pause()?;
            }
            self.playing = playing;
        }
        Ok(())
    }
    pub(super) fn toggle(&mut self) -> Result<()> {
        if self.player.is_none() {
            bail!("音乐尚未打开或播放已出错，请先刷新音乐");
        }
        self.set_playing(!self.playing)
    }
    pub(super) fn clear_preserving_files(&mut self, folder: &Path) -> Result<PathBuf> {
        let meta = fs::symlink_metadata(folder)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("音乐目录不是普通文件夹，未移动");
        }
        let parent = folder.parent().context("音乐文件夹没有父目录")?;
        let archive = tempfile::Builder::new()
            .prefix("music-removed-")
            .tempdir_in(parent)?;
        self.player = None;
        self.playing = false;
        let target = archive.path().join("musics");
        fs::rename(folder, &target).context("无法移出音乐文件夹，原文件未删除")?;
        let kept = archive.keep();
        self.files.clear();
        self.waiting.clear();
        self.current = None;
        self.progress = 0.0;
        fs::create_dir(folder)
            .with_context(|| format!("音乐已保存在 {}，但新文件夹创建失败", kept.display()))?;
        Ok(kept.join("musics"))
    }
    pub(super) fn game_changed(&mut self, started: bool, settings: &Settings) -> Result<()> {
        if settings.ui_music_stop {
            self.set_playing(!started)
        } else if settings.ui_music_start {
            self.set_playing(started)
        } else {
            Ok(())
        }
    }
}

fn collect_music(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![folder.to_path_buf()];
    let mut result = Vec::new();
    while let Some(folder) = pending.pop() {
        let metadata = match fs::symlink_metadata(&folder) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("读取音乐文件夹失败"),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("音乐文件夹不能是符号链接：{}", folder.display());
        }
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "wav" | "mp3" | "flac" | "m4a" | "aac" | "aiff" | "aif" | "ogg" | "wma"
                        )
                    })
            {
                result.push(entry.path());
            }
            if result.len() + pending.len() > 10_000 {
                bail!("音乐条目超过 10000 个，请缩小音乐文件夹范围");
            }
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use objc2::{rc::Retained, AllocAnyThread};
    use objc2_avf_audio::AVAudioPlayer;
    use objc2_foundation::{NSString, NSURL};
    pub(crate) struct Player(Retained<AVAudioPlayer>);
    impl Audio for Player {
        fn open(path: &Path) -> Result<Self> {
            let path = path.to_str().context("音乐路径不是有效 Unicode")?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            // The player retains its URL; all operations stay on the application's UI thread.
            let player =
                unsafe { AVAudioPlayer::initWithContentsOfURL_error(AVAudioPlayer::alloc(), &url) }
                    .map_err(|error| {
                        anyhow::anyhow!("系统音频解码失败：{}", error.localizedDescription())
                    })?;
            if !unsafe { player.prepareToPlay() } {
                bail!("系统无法准备播放此音频");
            }
            Ok(Self(player))
        }
        fn play(&mut self) -> Result<()> {
            if !unsafe { self.0.play() } {
                bail!("系统拒绝播放音频");
            }
            Ok(())
        }
        fn pause(&mut self) -> Result<()> {
            unsafe {
                self.0.pause();
            }
            Ok(())
        }
        fn volume(&mut self, volume: u16) -> Result<()> {
            unsafe {
                self.0.setVolume(f32::from(volume) / 1000.0);
            }
            Ok(())
        }
        fn position(&self) -> Result<(Duration, Duration, bool)> {
            unsafe {
                Ok((
                    Duration::from_secs_f64(self.0.currentTime().max(0.0)),
                    Duration::from_secs_f64(self.0.duration().max(0.0)),
                    self.0.isPlaying(),
                ))
            }
        }
    }
    impl Drop for Player {
        fn drop(&mut self) {
            unsafe {
                self.0.stop();
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use std::{
        os::windows::ffi::OsStrExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn mciSendStringW(
            command: *const u16,
            output: *mut u16,
            length: u32,
            callback: isize,
        ) -> u32;
        fn mciGetErrorStringW(error: u32, output: *mut u16, length: u32) -> i32;
    }
    static NEXT: AtomicU64 = AtomicU64::new(1);
    pub(crate) struct Player {
        alias: String,
    }
    fn send(command: &str) -> Result<String> {
        let command: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let mut output = [0_u16; 512];
        let code = unsafe {
            mciSendStringW(
                command.as_ptr(),
                output.as_mut_ptr(),
                output.len() as u32,
                0,
            )
        };
        if code != 0 {
            unsafe {
                mciGetErrorStringW(code, output.as_mut_ptr(), output.len() as u32);
            }
            bail!(
                "系统音频错误 {code}：{}",
                String::from_utf16_lossy(
                    &output[..output.iter().position(|c| *c == 0).unwrap_or(output.len())]
                )
            );
        }
        Ok(String::from_utf16_lossy(
            &output[..output.iter().position(|c| *c == 0).unwrap_or(output.len())],
        ))
    }
    impl Audio for Player {
        fn open(path: &Path) -> Result<Self> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.contains(&0) || wide.contains(&u16::from(b'"')) {
                bail!("音乐路径含不支持的字符");
            }
            let path = String::from_utf16(&wide).context("音乐路径不是有效 Unicode")?;
            let player = Self {
                alias: format!("pcl_music_{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            };
            // Digital-video MCI supports per-player volume; waveaudio does not.
            // https://learn.microsoft.com/windows/win32/multimedia/setaudio
            send(&format!(
                "open \"{path}\" type mpegvideo alias {}",
                player.alias
            ))?;
            send(&format!("set {} time format milliseconds", player.alias))?;
            Ok(player)
        }
        fn play(&mut self) -> Result<()> {
            send(&format!("play {}", self.alias)).map(|_| ())
        }
        fn pause(&mut self) -> Result<()> {
            send(&format!("pause {}", self.alias)).map(|_| ())
        }
        fn volume(&mut self, volume: u16) -> Result<()> {
            send(&format!("setaudio {} volume to {volume}", self.alias)).map(|_| ())
        }
        fn position(&self) -> Result<(Duration, Duration, bool)> {
            let position = send(&format!("status {} position", self.alias))?
                .trim()
                .parse::<u64>()?;
            let length = send(&format!("status {} length", self.alias))?
                .trim()
                .parse::<u64>()?;
            let mode = send(&format!("status {} mode", self.alias))?;
            Ok((
                Duration::from_millis(position),
                Duration::from_millis(length),
                mode.trim() == "playing",
            ))
        }
    }
    impl Drop for Player {
        fn drop(&mut self) {
            let _ = send(&format!("close {}", self.alias));
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod native {
    use super::*;
    pub(crate) struct Player;
    impl Audio for Player {
        fn open(_: &Path) -> Result<Self> {
            bail!("当前平台尚无系统音频后端")
        }
        fn play(&mut self) -> Result<()> {
            bail!("当前平台尚无系统音频后端")
        }
        fn pause(&mut self) -> Result<()> {
            Ok(())
        }
        fn volume(&mut self, _: u16) -> Result<()> {
            Ok(())
        }
        fn position(&self) -> Result<(Duration, Duration, bool)> {
            Ok((Duration::ZERO, Duration::ZERO, false))
        }
    }
}

// Fixed upstream ModBase.vb, Logo.IconMusic; original path retained.
const ICONMUSIC: &str = "M348.293565 716.53287V254.797913c0-41.672348 28.004174-78.358261 68.919652-90.37913L815.994435 40.826435c62.775652-18.610087 125.907478 26.579478 125.907478 89.933913v539.158261c8.013913 42.25113-8.94887 89.177043-47.014956 127.109565a232.848696 232.848696 0 0 1-170.785392 65.758609c-61.885217-2.938435-111.081739-33.435826-129.113043-80.050087-18.031304-46.614261-2.137043-102.177391 41.672348-145.853218a232.848696 232.848696 0 0 1 170.785391-65.80313c21.014261 1.024 40.514783 5.164522 57.878261 12.065391V233.338435c0-12.109913-10.551652-20.034783-20.569044-20.034783a24.620522 24.620522 0 0 0-5.787826 0.934957L439.785739 338.18713a19.545043 19.545043 0 0 0-14.825739 19.144348v438.984348H423.846957c11.53113 43.987478-5.164522 94.208-45.412174 134.322087a232.848696 232.848696 0 0 1-170.785392 65.758609c-61.885217-2.938435-111.081739-33.435826-129.113043-80.050087-18.031304-46.614261-2.137043-102.177391 41.672348-145.853218a232.848696 232.848696 0 0 1 170.785391-65.80313c20.791652 1.024 40.069565 5.075478 57.299478 11.842783z";

// Fixed upstream ModBase.vb, Logo.IconPlay; original path retained.
const ICONPLAY: &str = "M803.904 463.936a55.168 55.168 0 0 1 0 96.128l-463.616 264.448C302.848 845.888 256 819.136 256 776.448V247.616c0-42.752 46.848-69.44 84.288-48.064l463.616 264.384z";

pub(super) fn paint_icon(ui: &eframe::egui::Ui, rect: eframe::egui::Rect, playing: bool) {
    use eframe::egui;
    let key = egui::Id::new(("pcl-music-source-icon", playing));
    let texture = ui
        .ctx()
        .data_mut(|data| data.get_temp::<egui::TextureHandle>(key));
    let texture=texture.or_else(||{
        let path=if playing{ICONMUSIC}else{ICONPLAY};
        let svg=format!("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1024 1024'><path fill='white' d='{path}'/></svg>");
        let tree=resvg::usvg::Tree::from_str(&svg,&resvg::usvg::Options::default()).ok()?;
        let mut pixmap=resvg::tiny_skia::Pixmap::new(64,64)?;
        resvg::render(&tree,resvg::tiny_skia::Transform::from_scale(64.0/1024.0,64.0/1024.0),&mut pixmap.as_mut());
        let texture=ui.ctx().load_texture("music-source-icon",egui::ColorImage::from_rgba_premultiplied([64,64],pixmap.data()),egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|data|data.insert_temp(key,texture.clone()));Some(texture)
    });
    if let Some(texture) = texture {
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            crate::theme::palette(ui.ctx()).lightest,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        playing: bool,
        volume: u16,
    }
    impl Audio for Fake {
        fn open(path: &Path) -> Result<Self> {
            if path.file_stem().unwrap() == "broken" {
                bail!("invalid audio");
            }
            Ok(Self {
                playing: false,
                volume: 0,
            })
        }
        fn play(&mut self) -> Result<()> {
            self.playing = true;
            Ok(())
        }
        fn pause(&mut self) -> Result<()> {
            self.playing = false;
            Ok(())
        }
        fn volume(&mut self, v: u16) -> Result<()> {
            self.volume = v;
            Ok(())
        }
        fn position(&self) -> Result<(Duration, Duration, bool)> {
            Ok((Duration::from_secs(1), Duration::from_secs(4), self.playing))
        }
    }
    #[test]
    fn paused_start_volume_toggle_and_game_policies_are_real_backend_state() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.mp3"), b"fixture").unwrap();
        let mut settings = Settings {
            ui_music_auto: false,
            ..Default::default()
        };
        let mut state = PlayerState::<Fake>::default();
        state.tick(dir.path(), &settings).unwrap();
        assert!(!state.playing());
        assert_eq!(state.progress, 0.25);
        state.tick(dir.path(), &settings).unwrap();
        assert!(!state.playing());
        state.toggle().unwrap();
        assert!(state.player.as_ref().unwrap().playing);
        settings.ui_music_volume = 123;
        settings.ui_music_stop = true;
        state.tick(dir.path(), &settings).unwrap();
        assert_eq!(state.player.as_ref().unwrap().volume, 123);
        state.game_changed(true, &settings).unwrap();
        assert!(!state.playing());
        state.game_changed(false, &settings).unwrap();
        assert!(state.playing());
        settings.ui_music_stop = false;
        settings.ui_music_start = true;
        state.game_changed(false, &settings).unwrap();
        assert!(!state.playing());
    }
    #[test]
    fn invalid_track_is_reported_once_and_skipped_without_changing_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["broken.mp3", "z.wav"] {
            fs::write(dir.path().join(name), b"fixture").unwrap();
        }
        let settings = Settings {
            ui_music_random: false,
            ..Default::default()
        };
        let mut state = PlayerState::<Fake>::default();
        assert!(state
            .tick(dir.path(), &settings)
            .unwrap_err()
            .to_string()
            .contains("broken.mp3"));
        assert_eq!(state.title(), "z");
        assert!(state.playing());
        state.tick(dir.path(), &settings).unwrap();
        assert_eq!(fs::read(dir.path().join("broken.mp3")).unwrap(), b"fixture");
    }
    #[test]
    fn playlist_cycle_has_no_repeat_at_boundary_and_no_symlink_recursion() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.mp3", "b.mp3", "c.mp3"] {
            fs::write(dir.path().join(name), b"fixture").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let settings = Settings {
            ui_music_random: false,
            ..Default::default()
        };
        let mut state = PlayerState::<Fake>::default();
        state.refresh(dir.path(), &settings, true).unwrap();
        assert_eq!(state.title(), "a");
        state.next(&settings, true).unwrap();
        assert_eq!(state.title(), "b");
        state.next(&settings, true).unwrap();
        assert_eq!(state.title(), "c");
        state.next(&settings, true).unwrap();
        assert_eq!(state.title(), "a");
        assert_eq!(state.files.len(), 3);
    }

    #[test]
    fn clear_moves_entire_music_folder_without_deleting_original_bytes() {
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("musics");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a.mp3"), b"original audio").unwrap();
        fs::write(folder.join("notes.txt"), b"user notes").unwrap();
        let mut state = PlayerState::<Fake>::default();
        state.refresh(&folder, &Settings::default(), true).unwrap();
        let archive = state.clear_preserving_files(&folder).unwrap();
        assert_eq!(fs::read(archive.join("a.mp3")).unwrap(), b"original audio");
        assert_eq!(fs::read(archive.join("notes.txt")).unwrap(), b"user notes");
        assert_eq!(fs::read_dir(folder).unwrap().count(), 0);
        assert!(!state.visible());
        assert!(!state.playing());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "uses the native macOS audio device with a generated silent WAV"]
    fn native_silent_wav_plays_pauses_and_applies_volume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silence.wav");
        let samples = 44100_u32;
        let data_size = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&44100_u32.to_le_bytes());
        wav.extend_from_slice(&88200_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.resize(44 + data_size as usize, 0);
        fs::write(&path, wav).unwrap();
        let mut player = native::Player::open(&path).unwrap();
        player.volume(0).unwrap();
        player.play().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let (position, length, playing) = player.position().unwrap();
        assert!(playing);
        assert!(position > Duration::ZERO);
        assert!(length >= Duration::from_millis(990));
        player.pause().unwrap();
        assert!(!player.position().unwrap().2);
        player.volume(500).unwrap();
        player.play().unwrap();
        assert!(player.position().unwrap().2);
    }
}
