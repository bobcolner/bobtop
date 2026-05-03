//! Terminal lifecycle + render loop.
//!
//! - `init_terminal` enters raw mode + alternate screen and installs a
//!   panic hook that restores the terminal before propagating the panic.
//! - `restore_terminal` is the safe path on normal shutdown.
//! - `spawn_input_thread` shoves crossterm events onto an mpsc — the actual
//!   `event::read` is blocking, so it lives on its own OS thread.
//! - `run` is the 60Hz event loop: drain the bus + input, then redraw.

use std::io::{self, Stdout, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bobtop_core::DataBus;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::signal;
use tokio::sync::mpsc;

use crate::app::{App, ControlFlow};
use crate::ui;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

const FRAME_INTERVAL: Duration = Duration::from_millis(16); // ≈ 60fps

pub fn init_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

pub fn restore_terminal(term: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen, Show)?;
    term.show_cursor().ok();
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, Show);
        let _ = stdout.flush();
        original(info);
    }));
}

/// Spawn a blocking OS thread that polls crossterm and forwards events
/// onto the async runtime via an unbounded mpsc.
pub fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("bobtop-input".into())
        .spawn(move || loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => continue,
                Err(_) => return,
            }
        })
        .expect("spawn input thread");
    rx
}

/// Drive the terminal at ~60Hz: drain the bus + input, redraw, repeat.
pub async fn run(
    term: &mut Term,
    app: Arc<Mutex<App>>,
    bus: DataBus,
    mut input_rx: mpsc::UnboundedReceiver<Event>,
) -> io::Result<()> {
    let mut bus_rx = bus.subscribe();
    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                tracing::info!("ctrl-c received, shutting down");
                return Ok(());
            }
            _ = tick.tick() => {
                // Drain pending bus events. ALL event types apply — earlier
                // versions had a separate "inter-tick" select arm that
                // consumed events from the bus but only applied CPU/Mem/Process,
                // silently dropping Network and Disk events. Removed.
                loop {
                    match bus_rx.try_recv() {
                        Ok(ev) => {
                            let mut g = lock(&app);
                            g.apply_event(ev);
                        }
                        Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return Ok(()),
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "bus receiver lagged");
                        }
                    }
                }
                // Drain pending input.
                while let Ok(ev) = input_rx.try_recv() {
                    let flow = {
                        let mut g = lock(&app);
                        g.handle_input(ev)
                    };
                    if matches!(flow, ControlFlow::Quit) {
                        return Ok(());
                    }
                }
                // Render with a snapshot reference (lock held only for the draw).
                let g = lock(&app);
                term.draw(|f| ui::draw(f, &g))?;
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
