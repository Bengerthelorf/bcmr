#[cfg(unix)]
use signal_hook::consts::signal::{SIGCONT, SIGTSTP};
#[cfg(unix)]
use signal_hook::iterator::Signals;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(unix)]
use std::thread;

/// Tracks whether the TUI is currently suspended (backgrounded).
///
/// On SIGTSTP the signal thread disables raw mode, shows the cursor,
/// then raises SIGSTOP to truly stop the process. On SIGCONT it
/// re-enables raw mode if the process is in the foreground, and sets
/// the `suspended` flag accordingly so the render loop can skip draws.
pub fn install_suspend_handler() -> io::Result<Arc<AtomicBool>> {
    let suspended = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    {
        let suspended_clone = Arc::clone(&suspended);
        let mut signals = Signals::new([SIGTSTP, SIGCONT]).map_err(io::Error::other)?;

        thread::spawn(move || {
            use crossterm::cursor::Show;
            use crossterm::execute;
            use crossterm::terminal::disable_raw_mode;

            for sig in signals.forever() {
                match sig {
                    SIGTSTP => {
                        // Immediately clean up the terminal so the shell prompt looks normal.
                        let _ = execute!(std::io::stdout(), Show);
                        let _ = disable_raw_mode();
                        suspended_clone.store(true, Ordering::SeqCst);

                        // Actually stop the process (SIGSTOP cannot be caught).
                        unsafe {
                            libc::raise(libc::SIGSTOP);
                        }
                        // Execution resumes here after SIGCONT.
                    }
                    SIGCONT => {
                        // Check if we're back in the foreground.
                        let in_foreground = unsafe {
                            let fg = libc::tcgetpgrp(libc::STDIN_FILENO);
                            fg >= 0 && fg == libc::getpgrp()
                        };

                        if in_foreground {
                            use crossterm::cursor::Hide;
                            use crossterm::terminal::enable_raw_mode;
                            let _ = enable_raw_mode();
                            let _ = execute!(std::io::stdout(), Hide);
                            suspended_clone.store(false, Ordering::SeqCst);
                        }
                        // If backgrounded, stay suspended — the flag remains true.
                    }
                    _ => {}
                }
            }
        });
    }

    Ok(suspended)
}
