mod args;
mod audit;
mod config;
mod devel;
mod install;
mod meta;
mod ops;
mod pacman;
mod review;
mod rpc;
mod signals;
mod util;
mod vercmp;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Die quietly on a closed pipe (`aurgus -Ss foo | head`) like other CLI tools.
    // SAFETY: called before any other thread exists.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let result = args::parse(&raw).and_then(|a| {
        util::set_noconfirm(a.has_long("noconfirm"));
        ops::dispatch(a)
    });
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            util::error(&format!("{e:#}"));
            ExitCode::from(1)
        }
    }
}
