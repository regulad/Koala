//! The tray's state, kept on the async side: whether Warlock answers, and
//! the guest run that Connect starts ([`guest::run`]), turned into a
//! [`View`] for the windows.

use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::{JoinError, JoinHandle};
use warlock_client::{Client, PIPE};

use crate::guest::{self, Event, Options, Report};

/// How often Warlock is asked whether it's there, while nothing runs.
const POLL: Duration = Duration::from_secs(2);
/// What a paired iPhone is called when it didn't give its name.
const UNNAMED: &str = "iPhone";

/// The menu's first row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Warlock's API doesn't answer (not running, or no access to it).
    Unavailable,
    Disconnected,
    /// An iPhone has paired; its audio hasn't arrived yet.
    Connecting(String),
    /// Its audio is arriving.
    Connected(String),
}

impl Status {
    pub fn heading(&self) -> String {
        match self {
            Status::Unavailable => "Warlock service unavailable".into(),
            Status::Disconnected => "Disconnected".into(),
            Status::Connecting(name) => format!("Connecting to {name}…"),
            Status::Connected(name) => format!("Connected to {name}"),
        }
    }

    /// Whether Connect starts something: only from Disconnected.
    pub fn can_connect(&self) -> bool {
        *self == Status::Disconnected
    }
}

/// The QR window's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Qr {
    Hidden,
    /// Connect was chosen; the run hasn't made its code yet.
    Preparing,
    /// The code's URL.
    Code(String),
    /// The attempt failed, and why: shown until the window is closed or
    /// another attempt starts.
    Failed(String),
}

/// What the windows show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub status: Status,
    pub qr: Qr,
    /// Warlock's version, once it has answered.
    pub warlock: Option<String>,
}

impl Default for View {
    fn default() -> Self {
        Self { status: Status::Unavailable, qr: Qr::Hidden, warlock: None }
    }
}

/// What the windows ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Connect,
    /// The QR window was closed: stop trying.
    Cancel,
}

/// Runs the tray's state until `commands` closes, handing every changed
/// [`View`] to `publish`.
pub async fn run(mut commands: UnboundedReceiver<Command>, publish: impl Fn(&View) + Send) {
    let mut view = View::default();
    let mut task: Option<JoinHandle<Result<(), String>>> = None;
    let mut events: Option<UnboundedReceiver<Event>> = None;
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    publish(&view);
    loop {
        let before = view.clone();
        // Why the attempt failed, if it just did.
        let mut failed = None;
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Connect) if task.is_none() && view.status.can_connect() => {
                    let (tx, rx) = unbounded_channel();
                    let options = Options { wait: None, session: None, terminal: false, report: Report::to(tx) };
                    task = Some(tokio::spawn(guest::run(options)));
                    events = Some(rx);
                    view.qr = Qr::Preparing;
                }
                Some(Command::Connect) => {}
                Some(Command::Cancel) => {
                    stop(&mut task, &mut events);
                    view.status = Status::Disconnected;
                    view.qr = Qr::Hidden;
                }
                None => {
                    stop(&mut task, &mut events);
                    return;
                }
            },
            Some(event) = next(&mut events) => match event {
                Event::Qr(url) => view.qr = Qr::Code(url),
                Event::Paired(name) => view.status = Status::Connecting(name.unwrap_or_else(|| UNNAMED.into())),
                Event::Streaming => {
                    let name = match &view.status {
                        Status::Connecting(name) | Status::Connected(name) => name.clone(),
                        _ => UNNAMED.into(),
                    };
                    view.status = Status::Connected(name);
                    view.qr = Qr::Hidden;
                }
                Event::Ended(why) => {
                    stop(&mut task, &mut events);
                    view.status = Status::Disconnected;
                    failed = why;
                }
            },
            result = finished(&mut task) => {
                task = None;
                events = None;
                failed = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(e) if e.is_cancelled() => None,
                    Err(e) => Some(format!("Koala's connection failed: {e}")),
                };
                view.status = Status::Disconnected;
            }
            _ = poll.tick(), if task.is_none() => match Client::new(PIPE).info().await {
                Ok(info) => {
                    if view.status == Status::Unavailable {
                        view.status = Status::Disconnected;
                    }
                    view.warlock = Some(info.version);
                }
                Err(_) => view.status = Status::Unavailable,
            },
        }
        // An attempt that ended: its window says why, or goes.
        if task.is_none() && matches!(view.qr, Qr::Preparing | Qr::Code(_)) {
            view.qr = failed.map_or(Qr::Hidden, Qr::Failed);
        }
        if view != before {
            publish(&view);
        }
    }
}

/// Ends the run, and with it everything it started.
fn stop(task: &mut Option<JoinHandle<Result<(), String>>>, events: &mut Option<UnboundedReceiver<Event>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
    *events = None;
}

async fn next(events: &mut Option<UnboundedReceiver<Event>>) -> Option<Event> {
    match events {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn finished(task: &mut Option<JoinHandle<Result<(), String>>>) -> Result<Result<(), String>, JoinError> {
    match task {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_row_reads_as_asked() {
        assert_eq!(Status::Unavailable.heading(), "Warlock service unavailable");
        assert_eq!(Status::Disconnected.heading(), "Disconnected");
        assert_eq!(Status::Connecting("Parker's iPhone".into()).heading(), "Connecting to Parker's iPhone…");
        assert_eq!(Status::Connected("Parker's iPhone".into()).heading(), "Connected to Parker's iPhone");
    }

    #[test]
    fn connect_is_only_offered_when_disconnected() {
        assert!(Status::Disconnected.can_connect());
        assert!(!Status::Unavailable.can_connect());
        assert!(!Status::Connecting("x".into()).can_connect());
        assert!(!Status::Connected("x".into()).can_connect());
    }
}
