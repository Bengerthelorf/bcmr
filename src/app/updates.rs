use crate::cli::Commands;
use crate::commands;
use crate::config::{UpdateCheck, CONFIG};
use std::sync::mpsc;

pub(crate) fn background_update_check(
    command: &Commands,
) -> Option<mpsc::Receiver<Option<String>>> {
    if matches!(
        command,
        Commands::Update { .. }
            | Commands::Completions { .. }
            | Commands::CompleteRemote { .. }
            | Commands::Serve { .. }
            | Commands::Deploy { .. }
    ) {
        return None;
    }
    schedule_update_check(CONFIG.update_check, commands::update::check_for_update)
}

fn schedule_update_check<F>(mode: UpdateCheck, check: F) -> Option<mpsc::Receiver<Option<String>>>
where
    F: FnOnce() -> Option<String> + Send + 'static,
{
    match mode {
        UpdateCheck::Off => None,
        UpdateCheck::Quiet => {
            std::thread::spawn(move || {
                let _ = check();
            });
            None
        }
        UpdateCheck::Notify => {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(check());
            });
            Some(rx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::schedule_update_check;
    use crate::config::UpdateCheck;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn quiet_checks_without_returning_a_notification_channel() {
        let (ran_tx, ran_rx) = mpsc::channel();
        let result = schedule_update_check(UpdateCheck::Quiet, move || {
            ran_tx.send(()).unwrap();
            Some("9.9.9".to_string())
        });

        assert!(result.is_none());
        ran_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("quiet mode did not perform its background check");
    }

    #[test]
    fn notify_returns_the_background_result_and_off_does_not_run() {
        let notify = schedule_update_check(UpdateCheck::Notify, || Some("9.9.9".to_string()))
            .expect("notify mode must expose its result");
        assert_eq!(
            notify.recv_timeout(Duration::from_secs(1)).unwrap(),
            Some("9.9.9".to_string())
        );

        let (ran_tx, ran_rx) = mpsc::channel();
        assert!(schedule_update_check(UpdateCheck::Off, move || {
            ran_tx.send(()).unwrap();
            None
        })
        .is_none());
        assert!(ran_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
