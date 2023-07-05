use nostr_relay::{extensions, App};
use tracing::info;

#[actix_web::main]

async fn main() -> nostr_relay::Result<()> {
    tracing_subscriber::fmt::init();
    info!("Start relay server");
    let app_data = App::create(
        Some("../rnostr.example.toml"),
        true,
        Some("NOSTR".to_owned()),
        None,
    )?;
    app_data
        .add_extension(extensions::Metrics::new())
        .add_extension(extensions::Auth::new())
        .add_extension(extensions::Ratelimiter::new())
        .web_server()?
        .await?;
    info!("Relay server shutdown");
    Ok(())
}
