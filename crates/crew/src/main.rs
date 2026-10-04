use clap::Parser;

fn main() {
    let cli = crew::cli::Cli::parse();
    if let Err(error) = crew::cli::run(cli) {
        eprintln!("crew: {error:#}");
        std::process::exit(1);
    }
}
