//! The tray's state, kept on the async side: whether Warlock answers, and
//! the guest run that Connect starts ([`guest::run`]), turned into a
//! [`View`] for the windows.

use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::{JoinError, JoinHandle};
use warlock_client::types::{BluetoothState, Info, RadioState};
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
    /// Warlock answers, but this computer lacks something a connection
    /// needs, as Warlock reports it.
    Missing(Missing),
    Disconnected,
    /// An iPhone has paired; its audio hasn't arrived yet.
    Connecting(String),
    /// Its audio is arriving.
    Connected(String),
}

/// What a connection needs that Warlock reports missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// No Wi-Fi adapter for Warlock's radio (`radio.state` `absent`).
    WiFi,
    /// No Bluetooth adapter, or one without LE extended advertising, which
    /// Warlock's ask for the iPhone needs.
    Bluetooth5,
    /// The Bluetooth adapter's radio is off.
    BluetoothOn,
}

impl Status {
    pub fn heading(&self) -> String {
        match self {
            Status::Unavailable => "Warlock service unavailable".into(),
            Status::Missing(Missing::WiFi) => "No Wi-Fi adapter for Warlock".into(),
            Status::Missing(Missing::Bluetooth5) => "No Bluetooth 5 adapter".into(),
            Status::Missing(Missing::BluetoothOn) => "Bluetooth is off".into(),
            Status::Disconnected => "Disconnected".into(),
            Status::Connecting(name) => format!("Connecting to {name}…"),
            Status::Connected(name) => format!("Connected to {name}"),
        }
    }

    /// Whether Connect starts something: only from Disconnected.
    pub fn can_connect(&self) -> bool {
        *self == Status::Disconnected
    }

    /// The status while nothing runs, from what Warlock reports: the Wi-Fi
    /// adapter first, then Bluetooth. A Warlock that doesn't report
    /// Bluetooth (before 0.2.5) is taken at its word on the radio alone.
    pub fn idle(info: &Info) -> Status {
        if info.radio.state == RadioState::Absent {
            return Status::Missing(Missing::WiFi);
        }
        match info.bluetooth {
            Some(b) if b.state == BluetoothState::Absent || !b.extended_advertising => {
                Status::Missing(Missing::Bluetooth5)
            }
            Some(b) if b.state != BluetoothState::On => Status::Missing(Missing::BluetoothOn),
            _ => Status::Disconnected,
        }
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
                // A run that failed (a session's own ends report themselves).
                if let Some(e) = &failed {
                    crate::diagnostics::error(e);
                }
                view.status = Status::Disconnected;
            }
            // Not a transaction for the diagnostics: a health probe, every
            // 2 s while nothing runs (crate::diagnostics).
            _ = poll.tick(), if task.is_none() => match Client::new(PIPE).info().await {
                Ok(info) => {
                    // Nothing runs, so the row is the idle one.
                    view.status = Status::idle(&info);
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
        assert!(!Status::Missing(Missing::WiFi).can_connect());
        assert!(!Status::Missing(Missing::Bluetooth5).can_connect());
        assert!(!Status::Missing(Missing::BluetoothOn).can_connect());
        assert!(!Status::Connecting("x".into()).can_connect());
        assert!(!Status::Connected("x".into()).can_connect());
    }

    /// What's missing, as Warlock reports it: the Wi-Fi adapter first, then
    /// Bluetooth; a Warlock that doesn't report Bluetooth isn't held to it.
    #[test]
    fn the_idle_row_says_what_warlock_reports_missing() {
        use warlock_client::types::{Awdl, Bluetooth, Radio};
        let info = |radio: RadioState, bluetooth: Option<(BluetoothState, bool)>| Info {
            version: "0.2.5".into(),
            host_name: "pc".into(),
            adapter: None,
            radio: Radio { state: radio, address: None, channel: None, width_mhz: None },
            awdl: Awdl { master: None, peers: 0 },
            traffic: None,
            requestable: vec!["_rdlink._tcp".into()],
            bluetooth: bluetooth.map(|(state, extended_advertising)| Bluetooth { state, extended_advertising }),
        };
        let on = Some((BluetoothState::On, true));
        assert_eq!(Status::idle(&info(RadioState::Up, on)), Status::Disconnected);
        assert_eq!(Status::idle(&info(RadioState::Opening, on)), Status::Disconnected);
        assert_eq!(Status::idle(&info(RadioState::Absent, on)), Status::Missing(Missing::WiFi));
        assert_eq!(
            Status::idle(&info(RadioState::Absent, Some((BluetoothState::Absent, false)))),
            Status::Missing(Missing::WiFi),
            "Wi-Fi first"
        );
        assert_eq!(
            Status::idle(&info(RadioState::Up, Some((BluetoothState::Absent, false)))),
            Status::Missing(Missing::Bluetooth5)
        );
        assert_eq!(
            Status::idle(&info(RadioState::Up, Some((BluetoothState::On, false)))),
            Status::Missing(Missing::Bluetooth5),
            "Bluetooth without LE extended advertising"
        );
        assert_eq!(
            Status::idle(&info(RadioState::Up, Some((BluetoothState::Off, true)))),
            Status::Missing(Missing::BluetoothOn)
        );
        assert_eq!(
            Status::idle(&info(RadioState::Up, Some((BluetoothState::Unknown, true)))),
            Status::Missing(Missing::BluetoothOn),
            "a state Koala doesn't know isn't on"
        );
        assert_eq!(Status::idle(&info(RadioState::Up, None)), Status::Disconnected, "a Warlock before 0.2.5");
    }

    #[test]
    fn the_missing_rows_read_as_asked() {
        assert_eq!(Status::Missing(Missing::WiFi).heading(), "No Wi-Fi adapter for Warlock");
        assert_eq!(Status::Missing(Missing::Bluetooth5).heading(), "No Bluetooth 5 adapter");
        assert_eq!(Status::Missing(Missing::BluetoothOn).heading(), "Bluetooth is off");
    }
}
