use std::{
    fs,
    net::TcpListener,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn occupied_port_fails_startup_without_panicking() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let expected = TcpListener::bind(address).unwrap_err().to_string();
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("relay.toml");
    fs::write(
        &file,
        format!(
            "[data]\npath = {:?}\n[network]\nhost = \"127.0.0.1\"\nport = {}\n[thread]\nreader = 1\n",
            directory.path().join("db").to_str().unwrap(),
            address.port()
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
    let stderr = directory.path().join("stderr.log");
    let mut child = command
        .stdout(Stdio::null())
        .stderr(fs::File::create(&stderr)?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            panic!(
                "occupied port did not stop startup: {}",
                fs::read_to_string(&stderr)?
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait()?;
    let error = fs::read_to_string(&stderr)?;
    assert!(!status.success());
    assert!(error.contains(&expected), "{error}");
    assert!(!error.contains("panicked"), "{error}");
    Ok(())
}
