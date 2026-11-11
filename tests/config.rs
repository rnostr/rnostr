use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn invalid_configuration_prevents_startup() -> anyhow::Result<()> {
    for (text, expected, environment) in [
        ("[auth]\nenabled = \"yes\"", "[auth]", None),
        (
            "[auth]\nenabled = true",
            "[auth]",
            Some(("RNOSTR_AUTH__ENABLED", "bad")),
        ),
    ] {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("relay.toml");
        let path = directory.path().join("db");
        fs::write(
            &file,
            format!(
                "{text}\n[data]\npath = {:?}\n[network]\nport = 0\n",
                path.to_str().unwrap()
            ),
        )?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_rnostr"));
        command.args(["relay", "-c"]).arg(&file);
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("RNOSTR_")
            {
                command.env_remove(name);
            }
        }
        if let Some((name, value)) = environment {
            command.env(name, value);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                panic!("invalid configuration did not stop startup: {text}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output()?;
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "accepted {text}");
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("panicked"), "{error}");
    }
    Ok(())
}
