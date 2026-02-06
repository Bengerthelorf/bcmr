use signal_hook::consts::signal::{SIGCONT, SIGTSTP};
use signal_hook::iterator::Signals;
use std::io;
use std::sync::mpsc::{self, Receiver};
use std::thread;

#[derive(Debug, Clone, Copy)]
pub enum SuspendEvent {
    Suspend,
    Continue,
}

/// Install a background thread that forwards SIGTSTP/SIGCONT into a channel.
///
/// Intended for use by the TUI renderer to gracefully reset terminal state on Ctrl+Z
/// and resume appropriately on fg/bg.
pub fn install_signal_forwarder() -> io::Result<Receiver<SuspendEvent>> {
    let (tx, rx) = mpsc::channel();

    let mut signals =
        Signals::new([SIGTSTP, SIGCONT]).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    thread::spawn(move || {
        for sig in signals.forever() {
            match sig {
                SIGTSTP => {
                    let _ = tx.send(SuspendEvent::Suspend);
                }
                SIGCONT => {
                    let _ = tx.send(SuspendEvent::Continue);
                }
                _ => {}
            }
        }
    });

    Ok(rx)
}
