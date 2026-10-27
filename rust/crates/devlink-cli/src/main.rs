use clap::Parser;

mod cli;

#[tokio::main]
async fn main() {
    if let Err(error) = cli::run(cli::Cli::parse()).await {
        eprintln!("DevLink error: {error}");
        std::process::exit(1);
    }
}
