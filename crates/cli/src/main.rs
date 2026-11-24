//! `cockpit`: Alien Cockpit OSS from a terminal. See `cockpit --help`; the commands are in lib.rs.

fn main() {
    let code = cockpit_cli::run(std::env::args_os(), &mut std::io::stdout());
    std::process::exit(code);
}
