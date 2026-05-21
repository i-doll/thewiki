//! Repo-local automation entrypoint.
//!
//! TODO: add subcommands (e.g. `ci`, `migrate`, `gen-schema`) as the project
//! grows. For now this is just a placeholder so the workspace compiles.

fn main() {
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        Some(other) => {
            eprintln!("xtask: unknown subcommand '{other}'");
            std::process::exit(2);
        }
        None => {
            println!("xtask: no subcommand given (none implemented yet)");
        }
    }
}
