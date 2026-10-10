use super::*;
use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

// In-memory authenticated fixtures are confined to unit tests. The real CLI has
// no switch, token argument or file import that can replace account verification.
fn fixture(body: &str) -> (tempfile::TempDir, RuntimeContext, LaunchArgs, Session) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("game with spaces");
    fs::create_dir_all(root.join("versions/fixture")).unwrap();
    fs::write(root.join("versions/fixture/fixture.json"), json!({
        "id":"fixture", "mainClass":"Fixture", "libraries":[],
        "minecraftArguments":"--username ${auth_player_name} --accessToken ${auth_access_token}",
        "javaVersion":{"majorVersion":21}
    }).to_string()).unwrap();
    fs::write(
        root.join("versions/fixture/fixture.jar"),
        b"not a real game",
    )
    .unwrap();
    let java = directory.path().join("fake java");
    fs::write(&java, format!("#!/bin/sh\nif [ \"$1\" = '-XshowSettings:properties' ]; then\n printf 'java.version = 21.0.7\\nos.arch = {}\\n'\n exit 0\nfi\n{body}\n", std::env::consts::ARCH)).unwrap();
    fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();
    let settings = config::Settings {
        game_root: root.clone(),
        memory_auto: false,
        disable_java_wrapper: true,
        disable_lwjgl_unsafe_agent: true,
        ..Default::default()
    };
    let cx = RuntimeContext {
        root,
        settings,
        config_path: directory.path().join("settings.json"),
        platform: Platform::current(),
        cancel: Arc::new(AtomicBool::new(false)),
        output: Output { json: true },
    };
    let args = LaunchArgs {
        version: "fixture".into(),
        java: Some(java),
        name: None,
        account: None,
        memory: Some(768),
        server: None,
    };
    let session = Session {
        username: "Fixture".into(),
        uuid: "0123456789abcdef0123456789abcdef".into(),
        user_type: "msa".into(),
        access_token: "private-fixture-token".into(),
    };
    (directory, cx, args, session)
}

#[test]
fn authenticated_plan_honors_memory_override_without_mutating_preferences() {
    let (_directory, cx, args, session) = fixture("touch SHOULD-NOT-RUN");
    config::save_instance_settings(
        &cx.root,
        "fixture",
        &config::InstanceSettings {
            memory_auto: true,
            memory_mb: Some(2048),
            ..Default::default()
        },
    )
    .unwrap();
    let path = config::instance_settings_path(&cx.root, "fixture").unwrap();
    let before = fs::read(&path).unwrap();
    let (plan, _) = plan_with_session(&cx, args, false, session).unwrap();
    assert!(plan.args.contains(&"-Xmx768M".into()));
    assert!(!plan.redacted_command().contains("private-fixture-token"));
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(!plan.cwd.join("SHOULD-NOT-RUN").exists());
}

#[test]
fn game_runner_reports_real_exit_code() {
    let (_directory, cx, args, session) = fixture("exit 23");
    let (plan, session) = plan_with_session(&cx, args, true, session).unwrap();
    let error = run(&cx, plan, session).unwrap_err();
    assert_eq!(error.downcast_ref::<GameExit>().unwrap().0, 23);
}

#[test]
fn cancellation_stops_only_the_owned_game() {
    let (_directory, cx, args, session) = fixture("echo $$ > game-pid; exec sleep 30");
    let (plan, session) = plan_with_session(&cx, args, true, session).unwrap();
    let marker = plan.cwd.join("game-pid");
    let cancel = cx.cancel.clone();
    let mut command = Command::new("sleep");
    command.arg("30");
    own_process_group(&mut command);
    let mut sentinel = OwnedChild(command.spawn().unwrap(), true);
    let runner = thread::spawn(move || run(&cx, plan, session));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    let pid = fs::read_to_string(marker).unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(runner
        .join()
        .unwrap()
        .unwrap_err()
        .is::<OperationCancelled>());
    assert!(sentinel.0.try_wait().unwrap().is_none());
    assert!(!Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success());
}

#[test]
fn runner_rejects_an_invalid_microsoft_session_without_falling_back_to_offline() {
    let (_directory, cx, args, session) = fixture("touch SHOULD-NOT-RUN");
    let (plan, mut session) = plan_with_session(&cx, args, false, session).unwrap();
    let marker = plan.cwd.join("SHOULD-NOT-RUN");
    session.access_token = "0".into();
    let error = run(&cx, plan, session).unwrap_err();
    assert!(error.to_string().contains("登录身份无效"));
    assert!(!marker.exists());
}
