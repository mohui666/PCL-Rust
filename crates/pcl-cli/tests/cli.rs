use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    directory: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("game with spaces");
        let version = root.join("versions/fixture");
        fs::create_dir_all(&version).unwrap();
        fs::write(version.join("fixture.json"), json!({"id":"fixture","type":"release","mainClass":"Fixture",
            "minecraftArguments":"--username ${auth_player_name} --accessToken ${auth_access_token}","libraries":[],
            "javaVersion":{"majorVersion":21}}).to_string()).unwrap();
        fs::write(
            version.join("fixture.jar"),
            b"fixture jar; never executed by a JVM",
        )
        .unwrap();
        let config = directory.path().join("isolated-settings.json");
        fs::write(
            &config,
            json!({"game_root":root,"offline_name":"Fixture","memory_auto":false,
            "disable_java_wrapper":true,"disable_lwjgl_unsafe_agent":true})
            .to_string(),
        )
        .unwrap();
        Self {
            directory,
            root,
            config,
        }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_pcl-cli"));
        c.args(["--headless", "--json", "--root"])
            .arg(&self.root)
            .arg("--config")
            .arg(&self.config)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY");
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn jar(&self) -> PathBuf {
        let file = self.directory.path().join("fixture-mod.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&file).unwrap());
        zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"schemaVersion":1,"id":"fixture","version":"1.0","name":"Fixture"}"#)
            .unwrap();
        zip.finish().unwrap();
        file
    }
    #[cfg(unix)]
    fn java(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.directory.path().join("fake java");
        fs::write(&path,format!("#!/bin/sh\nif [ \"$1\" = '-XshowSettings:properties' ]; then\n printf 'java.version = 21.0.7\\nos.arch = {}\\n'\n exit 0\nfi\n{}\n",std::env::consts::ARCH,body)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn help_and_invalid_arguments_need_neither_display_nor_settings() {
    let output = Command::new(env!("CARGO_BIN_EXE_pcl-cli"))
        .args(["--config", "/does/not/exist.json", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("install-loader"));
    for args in [
        vec![],
        vec!["--headless"],
        vec!["nonsense"],
        vec!["install-loader", "unknown", "1.21.1", "1"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_pcl-cli"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
    }
}

#[test]
fn list_is_json_and_read_only_with_relative_root_rejected() {
    let f = Fixture::new();
    let before = fs::read(&f.config).unwrap();
    assert_eq!(f.ok(&["list"])[0]["id"], "fixture");
    assert_eq!(before, fs::read(&f.config).unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_pcl-cli"))
        .args(["--root", "relative", "--config"])
        .arg(&f.config)
        .arg("list")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

#[test]
fn mod_lifecycle_uses_shared_game_directory_and_reversible_backup() {
    let f = Fixture::new();
    pcl_core::config::save_instance_settings(
        &f.root,
        "fixture",
        &pcl_core::config::InstanceSettings {
            isolated: false,
            ..Default::default()
        },
    )
    .unwrap();
    let jar = f.jar();
    assert_eq!(
        f.ok(&["mod", "import", "fixture", text(&jar)])["imported"],
        1
    );
    assert!(f.root.join("mods/fixture-mod.jar").is_file());
    assert!(!f.root.join("instances").exists());
    assert_eq!(f.ok(&["mods", "fixture"])[0]["enabled"], true);
    f.ok(&["mod", "disable", "fixture", "fixture-mod.jar"]);
    assert_eq!(f.ok(&["mod", "list", "fixture"])[0]["enabled"], false);
    f.ok(&["mod", "enable", "fixture", "fixture-mod.jar.disabled"]);
    let removed = f.ok(&["mod", "remove", "fixture", "fixture-mod.jar"]);
    assert_eq!(removed["removed"], 1);
    assert_eq!(f.ok(&["mods", "fixture"]), json!([]));
    f.ok(&[
        "mod",
        "restore",
        "fixture",
        removed["backup"].as_str().unwrap(),
    ]);
    assert_eq!(
        fs::read(f.root.join("mods/fixture-mod.jar")).unwrap(),
        fs::read(jar).unwrap()
    );
}

#[test]
fn import_conflict_and_path_traversal_leave_files_intact() {
    let f = Fixture::new();
    let jar = f.jar();
    f.ok(&["mod", "import", "fixture", text(&jar)]);
    let before = fs::read(f.root.join("versions/fixture/mods/fixture-mod.jar")).unwrap();
    assert!(!f
        .run(&["mod", "import", "fixture", text(&jar)])
        .status
        .success());
    assert!(!f
        .run(&["mod", "remove", "fixture", "../../fixture.jar"])
        .status
        .success());
    assert!(!f
        .run(&["instance", "rename", "fixture", "../escape"])
        .status
        .success());
    assert_eq!(
        fs::read(f.root.join("versions/fixture/mods/fixture-mod.jar")).unwrap(),
        before
    );
    assert!(f.root.join("versions/fixture/fixture.jar").exists());
}

#[test]
fn invalid_resource_target_fails_before_any_network_or_download() {
    let f = Fixture::new();
    let out = f.run(&["resource", "install", "unused", "fixture"]);
    assert_eq!(out.status.code(), Some(1));
    let event: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(event["message"].as_str().unwrap().contains("Mod 加载器"));
    assert!(!f.root.join("versions/fixture/mods").exists());
    assert!(!f
        .run(&["resource", "install", "unused", "fixture", "--kind", "datapack"])
        .status
        .success());
}

#[test]
fn log_import_and_export_redact_credentials_without_opening_dialogs() {
    let f = Fixture::new();
    let input = f.directory.path().join("crash.log");
    let output = f.directory.path().join("report.zip");
    fs::write(
        &input,
        "accessToken=PRIVATE-TOKEN-123\njava.lang.OutOfMemoryError\n",
    )
    .unwrap();
    let report = f.ok(&["logs", "--file", text(&input), "--export", text(&output)]);
    assert!(!report.to_string().contains("PRIVATE-TOKEN"));
    assert!(!report["report"]["findings"].as_array().unwrap().is_empty());
    assert!(output.is_file());
}

#[test]
fn instance_settings_are_validated_and_rename_preserves_user_data() {
    let f = Fixture::new();
    let settings = f.directory.path().join("instance.json");
    fs::write(&settings, r#"{"memory_mb":1}"#).unwrap();
    assert!(!f
        .run(&[
            "instance",
            "settings",
            "fixture",
            "--apply",
            text(&settings)
        ])
        .status
        .success());
    fs::write(&settings, r#"{"memory_mb":1024,"description":"test"}"#).unwrap();
    f.ok(&[
        "instance",
        "settings",
        "fixture",
        "--apply",
        text(&settings),
    ]);
    fs::write(f.root.join("versions/fixture/user-data.txt"), "keep").unwrap();
    f.ok(&["instance", "rename", "fixture", "renamed"]);
    assert_eq!(
        f.ok(&["instance", "settings", "renamed"])["description"],
        "test"
    );
    assert_eq!(
        fs::read_to_string(f.root.join("versions/renamed/user-data.txt")).unwrap(),
        "keep"
    );
}

#[cfg(unix)]
#[test]
fn unauthenticated_launch_plan_and_script_never_execute_java_or_change_settings() {
    let f = Fixture::new();
    let marker = f.root.join("versions/fixture/SHOULD-NOT-RUN");
    let java = f.java("touch SHOULD-NOT-RUN");
    let script = f.directory.path().join("launch.command");
    let before = fs::read(&f.config).unwrap();
    for command in ["launch", "plan", "export-script"] {
        for offline_name in [false, true] {
            let mut args = vec![command, "fixture", "--java", text(&java)];
            if command == "export-script" {
                args.push(text(&script));
            }
            if offline_name {
                args.extend(["--name", "Tester"]);
            }
            let out = f.run(&args);
            assert_eq!(out.status.code(), Some(1));
            assert!(out.stdout.is_empty());
            assert!(String::from_utf8_lossy(&out.stderr).contains("离线登录已禁用"));
            assert!(!marker.exists());
            assert!(!script.exists());
            assert_eq!(fs::read(&f.config).unwrap(), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn instance_preferences_cannot_enable_offline_launch() {
    let f = Fixture::new();
    let java = f.java("touch SHOULD-NOT-RUN");
    for mode in [
        pcl_core::config::LoginRequirement::Any,
        pcl_core::config::LoginRequirement::Microsoft,
        pcl_core::config::LoginRequirement::Offline,
    ] {
        pcl_core::config::save_instance_settings(
            &f.root,
            "fixture",
            &pcl_core::config::InstanceSettings {
                login_requirement: mode,
                ..Default::default()
            },
        )
        .unwrap();
        let out = f.run(&["launch", "fixture", "--java", text(&java)]);
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stderr).contains("离线登录已禁用"));
        assert!(!f.root.join("versions/fixture/SHOULD-NOT-RUN").exists());
    }
}
