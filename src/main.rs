mod api;
mod auth;
mod command;
mod cookies;
mod error;
mod flv;
mod listen;
mod live;
mod payload;
mod record;
mod response;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    command::run().await
}
