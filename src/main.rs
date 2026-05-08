use unspace::cli;

fn main() {
    cli::init_logging("info");
    if let Err(error) = cli::run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
