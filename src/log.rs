//! Minimal internal logging helper.
//!
//! Keeps formatting consistent without adding external dependencies.

fn emit(level: &str, subsystem: &str, message: &str) {
    eprintln!("[cratond][{level}][{subsystem}] {message}");
}

pub fn raw(message: &str) {
    eprintln!("{message}");
}

pub fn info(subsystem: &str, message: &str) {
    emit("INFO", subsystem, message);
}

pub fn warn(subsystem: &str, message: &str) {
    emit("WARN", subsystem, message);
}

pub fn error(subsystem: &str, message: &str) {
    emit("ERROR", subsystem, message);
}
