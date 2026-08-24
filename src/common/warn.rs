use std::io::{self, IsTerminal};

pub(crate) fn warn(message: &str) {
    if io::stderr().is_terminal() {
        eprintln!("\x1b[33mwarning:\x1b[0m {message}");
    } else {
        eprintln!("warning: {message}");
    }
}
