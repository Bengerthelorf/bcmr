use crate::cli::Commands;
use crate::commands;
use crate::config::{UpdateCheck, CONFIG};
use std::sync::mpsc;

pub(crate) fn background_update_check(
    command: &Commands,
) -> Option<mpsc::Receiver<Option<String>>> {
    if matches!(
        command,
        Commands::Update
            | Commands::Completions { .. }
            | Commands::CompleteRemote { .. }
            | Commands::Serve { .. }
            | Commands::Deploy { .. }
    ) {
        return None;
    }
    if CONFIG.update_check != UpdateCheck::Notify {
        return None;
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(commands::update::check_for_update());
    });
    Some(rx)
}
