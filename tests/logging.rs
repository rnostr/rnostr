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
