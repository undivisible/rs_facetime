#[cfg(all(feature = "cli", not(target_os = "macos")))]
fn main() {
    eprintln!("rs_facetime requires macOS (FaceTime.app + private-api dylib)");
    std::process::exit(1);
}

#[cfg(all(feature = "cli", target_os = "macos"))]
fn main() {
    use clap::Parser;
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    #[path = "../cli/mod.rs"]
    mod cli;

    let cli = cli::Cli::parse();
    if let Err(error) = cli::run(cli) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
