use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn cli_logging_defaults_and_overrides() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let config = directory.path().join("missing.toml");
    let invalid_db = directory.path().join("not-a-directory");
    fs::write(&invalid_db, "")?;
    for (subcommand, filter, logs_info) in [
        ("relay", None, true),
        ("relay", Some(""), true),
        ("relay", Some("rnostr=invalid-level"), true),
        ("relay", Some("off"), false),
        ("relay", Some("unrelated=debug"), true),
        ("relay", Some("off,rnostr::relay=info"), true),
        ("relay", Some("off,rnostr=invalid-level"), true),
        ("import", Some("rnostr=invalid-level"), false),
        ("export", Some("rnostr=invalid-level"), false),
        ("bench", Some("rnostr=invalid-level"), false),
        ("delete", Some("rnostr=invalid-level"), false),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rnostr"));
        command.arg(subcommand).stdin(Stdio::null());
        if subcommand == "relay" {
            command.arg("-c").arg(&config);
        } else {
            command.arg(&invalid_db);
        }
        command.env_remove("RUST_LOG");
        command.env_remove("NO_COLOR");
        if let Some(filter) = filter {
            command.env("RUST_LOG", filter);
        }
        let stdout_path = directory.path().join("stdout.log");
        let stderr_path = directory.path().join("stderr.log");
        let mut child = command
            .stdout(fs::File::create(&stdout_path)?)
            .stderr(fs::File::create(&stderr_path)?)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                panic!("{subcommand} did not exit for RUST_LOG={filter:?}");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        let stdout = fs::read_to_string(stdout_path)?;
        let stderr = fs::read_to_string(stderr_path)?;
        assert!(stdout.is_empty(), "{subcommand}: {stdout}");
        assert!(!stderr.contains('\x1b'), "{subcommand}: {stderr:?}");
        if subcommand == "relay" {
            assert!(stderr.contains("missing.toml"), "{stderr}");
        }
        if filter.is_some_and(|value| value.contains("invalid-level")) {
            assert!(stderr.contains("Ignoring RUST_LOG="), "{stderr}");
        }
        assert_eq!(
            stderr.contains("Start relay server"),
            logs_info,
            "{subcommand} {filter:?}: {stderr}"
        );
    }
    Ok(())
}

#[test]
fn relay_preserves_the_callers_logging_subscriber() -> anyhow::Result<()> {
    tracing::subscriber::set_global_default(tracing_subscriber::registry())?;
    let directory = tempfile::tempdir()?;
    let config = directory.path().join("missing.toml");
    for _ in 0..2 {
        let error = rnostr::relay(&config, false).unwrap_err();
        assert!(error.to_string().contains("missing.toml"), "{error}");
        tracing::dispatcher::get_default(|dispatch| {
            assert!(dispatch.is::<tracing_subscriber::Registry>());
        });
    }
    Ok(())
}
