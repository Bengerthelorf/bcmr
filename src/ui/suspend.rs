#[cfg(unix)]
use signal_hook::consts::signal::{SIGCONT, SIGTSTP};
#[cfg(unix)]
use signal_hook::iterator::Signals;
use std::io;
use std::sync::mpsc::{self, Receiver};
#[cfg(unix)]
use std::thread;

#[derive(Debug, Clone, Copy)]
pub enum SuspendEvent {
    Suspend,
    Continue,
}

/// Install a background thread that forwards SIGTSTP/SIGCONT into a channel.
///
/// On non-Unix platforms this is a no-op that returns an empty receiver.
pub fn install_signal_forwarder() -> io::Result<Receiver<SuspendEvent>> {
    let (tx, rx) = mpsc::channel();

    #[cfg(unix)]
    {
        let mut signals = Signals::new([SIGTSTP, SIGCONT])
            .map_err(io::Error::other)?;

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
    }

    #[cfg(not(unix))]
    drop(tx);

    Ok(rx)
}
