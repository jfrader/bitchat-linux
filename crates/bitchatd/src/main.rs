use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BITCHAT_LOG")
                .unwrap_or_else(|_| "bitchatd=info".into()),
        )
        .without_time()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("bitchatd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    bitchatd::run_daemon().await
}
