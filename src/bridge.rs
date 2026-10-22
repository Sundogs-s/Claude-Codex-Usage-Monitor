//! Private stdio bridge: one bundled native client owns one collector process.
//! No listening ports, model calls, credentials in arguments, or raw auth output.
use crate::{claude, cursor, monitor, presentation, state};
use serde::Deserialize;
use std::{io::{BufRead, Read, Write}, sync::mpsc, time::Duration};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Refresh {},
    SetService { service: String, enabled: bool },
    SetRefresh { seconds: u64 },
    SetCookie { cookie: String },
    ClearCookie {},
    Quit {},
}

enum Event { Command(Command), Invalid, Changed, Closed }

fn apply(command: Command, shared: &state::SharedState) -> Result<bool, &'static str> {
    match command {
        Command::Quit {} => return Ok(false),
        Command::Refresh {} => { claude::reset_refresh_attempts(); monitor::wake_pollers(); }
        Command::SetService { service, enabled } => {
            let mut s = shared.lock();
            match service.as_str() {
                "claude" => s.settings.show_claude = enabled,
                "codex" => s.settings.show_codex = enabled,
                "cursor" => s.settings.show_cursor = enabled,
                _ => return Err("Unknown service"),
            }
            state::save_settings(&s.settings);
            drop(s);
            monitor::wake_pollers();
        }
        Command::SetRefresh { seconds } => {
            let rate = match seconds { 5 => state::RefreshRate::Secs5, 60 => state::RefreshRate::Min1,
                300 => state::RefreshRate::Min5, _ => return Err("Unsupported refresh interval") };
            let mut s = shared.lock();
            s.settings.refresh_rate = rate;
            state::save_settings(&s.settings);
            drop(s);
            monitor::set_refresh_secs(seconds);
        }
        Command::SetCookie { cookie } => {
            if cookie.trim().is_empty() || cookie.len() > 8192 || !cookie.is_ascii() || cookie.contains(['\r', '\n', '\0']) {
                return Err("Cookie must be a nonempty single line, at most 8192 bytes");
            }
            cursor::save_cookie(&cookie).map_err(|_| "Cannot save Cursor Cookie")?;
            monitor::wake_pollers();
        }
        Command::ClearCookie {} => {
            cursor::clear_cookie();
            let mut s = shared.lock();
            s.cursor = state::CursorUsage::default();
            s.cursor_error = "Cookie not configured".into();
        }
    }
    Ok(true)
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--version") {
        println!("Usage Monitor core {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if !args.iter().any(|s| s == "--stdio" || s == "--snapshot") {
        return Err("Launch UsageMonitor.app, or use --stdio / --snapshot for diagnostics".into());
    }
    let shared = state::new_shared();
    shared.lock().settings = state::load_settings();
    let mut output = std::io::stdout().lock();
    if args.iter().any(|s| s == "--snapshot") {
        serde_json::to_writer(&mut output, &presentation::snapshot(&shared.lock()))?;
        writeln!(output)?;
        return Ok(());
    }
    let (tx, rx) = mpsc::channel();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            let mut line = Vec::new();
            // Bound every command before decoding; no secret-bearing parse errors are emitted.
            let read = (&mut input).take(32769).read_until(b'\n', &mut line);
            if !matches!(read, Ok(n) if n > 0) { break; }
            if line.len() > 32768 || !line.ends_with(b"\n") {
                let _ = input_tx.send(Event::Invalid);
                break;
            }
            let event = serde_json::from_slice(&line).map(Event::Command).unwrap_or(Event::Invalid);
            if input_tx.send(event).is_err() { return; }
        }
        let _ = input_tx.send(Event::Closed);
    });
    monitor::set_refresh_secs(shared.lock().settings.refresh_rate.secs());
    // Fixture mode is only for local protocol tests and never starts collectors.
    let _monitor = if args.iter().any(|s| s == "--fixture") { None } else {
        Some(monitor::spawn(shared.clone(), move || { let _ = tx.send(Event::Changed); }))
    };
    let mut message = None;
    loop {
        let mut view = presentation::snapshot(&shared.lock());
        view.message = message.take();
        if serde_json::to_writer(&mut output, &view).is_err() || writeln!(output).is_err() || output.flush().is_err() {
            break;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Event::Command(command)) => match apply(command, &shared) {
                Ok(false) => break,
                Ok(true) => {},
                Err(error) => message = Some(error.to_owned()),
            },
            Ok(Event::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Event::Invalid) => message = Some("Invalid command".into()),
            _ => {},
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_rejects_unknown_actions_and_fields() {
        for input in [r#"{"action":"run_model"}"#, r#"{"action":"refresh","prompt":"never"}"#] {
            assert!(serde_json::from_str::<Command>(input).is_err());
        }
        let state = state::new_shared();
        assert!(apply(Command::SetRefresh { seconds: 1 }, &state).is_err());
        assert!(apply(Command::SetService { service: "unknown".into(), enabled: true }, &state).is_err());
        assert!(apply(Command::SetCookie { cookie: "a\r\nb".into() }, &state).is_err());
    }
}
