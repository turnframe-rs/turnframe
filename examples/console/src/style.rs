//! Terminal colour: what a user of a real surface would read in bright white, and what
//! the console adds to explain the turn in muted colours. Off when `NO_COLOR` is set or
//! the output is not a terminal.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, text: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// Text the user would see: the reply, answers, receipts, notices, cards.
pub fn shown(text: &str) -> String {
    paint("1;97", text)
}

/// A step a model wrote in the user's words, which a surface shows while the turn runs.
pub fn said(text: &str) -> String {
    paint("3;97", text)
}

/// What the console adds to explain the turn.
pub fn muted(text: &str) -> String {
    paint("90", text)
}

/// Something that went wrong on the way: a refused answer, a unit not understood.
pub fn warning(text: &str) -> String {
    paint("33", text)
}

/// The label of a diagnostic line.
pub fn tag(text: &str) -> String {
    paint("36", &format!("{text:<11}"))
}

/// The label of a block the user would see, coloured by kind.
pub fn block(kind: &str) -> String {
    let code = match kind {
        "assistant" => "1;32",
        "answer" => "1;36",
        "receipt" => "32",
        "notice" => "33",
        "card" => "1;35",
        _ => "37",
    };
    paint(code, &format!("{kind:<11}"))
}

/// The user's prompt.
pub fn prompt() -> String {
    paint("1;34", "you>")
}

/// A heading.
pub fn heading(text: &str) -> String {
    paint("1", text)
}
