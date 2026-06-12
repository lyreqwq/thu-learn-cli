mod api;
mod browser_login;
mod cache;
mod cli;
mod client;
mod models;
mod paths;

#[tokio::main]
async fn main() {
    if let Err(e) = cli::run().await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
