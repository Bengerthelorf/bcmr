#[cfg(unix)]
use signal_hook::consts::signal::{SIGCONT, SIGTSTP};
#[cfg(unix)]
use signal_hook::iterator::Signals;
use std::io;
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
#[cfg(unix)]
use std::thread;

#[cfg(unix)]
pub fn suspend_now(suspended: &AtomicBool) {
    use crossterm::cursor::Show;
    use crossterm::execute;
    use crossterm::terminal::disable_raw_mode;

    let _ = execute!(std::io::stdout(), Show);
    let _ = disable_raw_mode();
    suspended.store(true, Ordering::SeqCst);

    unsafe {
        libc::raise(libc::SIGSTOP);
    }
}

#[cfg(not(unix))]
pub fn suspend_now(_suspended: &AtomicBool) {}

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
                        let _ = execute!(std::io::stdout(), Show);
                        let _ = disable_raw_mode();
                        suspended_clone.store(true, Ordering::SeqCst);

                        unsafe {
                            libc::raise(libc::SIGSTOP);
                        }
                    }
                    SIGCONT => {
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
                    }
                    _ => {}
                }
            }
        });
    }

    Ok(suspended)
}
