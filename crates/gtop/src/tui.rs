//! Terminal lifecycle + render loop.
//!
//! - `init_terminal` enters raw mode + alternate screen and installs a
//!   panic hook that restores the terminal before propagating the panic.
//! - `restore_terminal` is the safe path on normal shutdown.
//! - `spawn_input_thread` shoves crossterm events onto an mpsc — the actual
//!   `event::read` is blocking, so it lives on its own OS thread.
//! - `run` is event-driven: it wakes on a bus event, an input event, or a
//!   slow heartbeat, then redraws only if `App::take_dirty()` is true.
//!   At idle (no metrics arriving, no keystrokes) this draws zero frames,
//!   matching btop's "draw on change" cost model.

use std::io::{self, Stdout, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::{Box as BoxKind, DataBus};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, DisableFocusChange, EnableFocusChange, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::signal;
use tokio::sync::mpsc;

use crate::app::{App, ControlFlow};
use crate::ui;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Slow heartbeat: ensures we still wake up periodically even with no bus
/// activity (so future time-derived UI like a clock or uptime overlay can
/// repaint). Independent of metric tick — just a "are we still alive" tick.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);

pub fn init_terminal() -> io::Result<Term> {
    // Enable raw mode under a guard. If anything between here and the final
    // `Terminal::new` returns Err, the guard's Drop disables raw mode so the
    // user's terminal isn't left mangled (would otherwise need `reset` to
    // recover). Once we successfully build the Terminal, we `disarm` the
    // guard — restore_terminal() owns the cleanup from then on.
    enable_raw_mode()?;
    let guard = RawModeGuard { armed: true };
    let mut stdout = io::stdout();
    // EnableFocusChange so terminal multiplexers (tmux with focus-events
    // on, kitty, alacritty, et) deliver Event::FocusGained on session
    // resume — that's our cue to force-clear the screen and repaint
    // because ratatui's diff buffer no longer matches reality.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableFocusChange,
        Hide,
        // Tab / window title: just "gtop" while we own the terminal.
        // Window managers and tmux pick this up via OSC 0; the file
        // browser overrides it to "gfb · <cwd>" when launched
        // and we re-set it on resume via re_init_terminal.
        SetTitle("gtop"),
    )?;
    install_panic_hook();
    let term = Terminal::new(CrosstermBackend::new(stdout))?;
    guard.disarm();
    Ok(term)
}

/// Drop-guard that disables raw mode unless `disarm()` was called. Used by
/// `init_terminal` to recover from partial-init failures without leaving
/// the terminal in raw mode.
struct RawModeGuard {
    armed: bool,
}

impl RawModeGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), DisableFocusChange, LeaveAlternateScreen, Show);
        }
    }
}

pub fn restore_terminal(term: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableFocusChange, LeaveAlternateScreen, Show)?;
    term.show_cursor().ok();
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableFocusChange, LeaveAlternateScreen, Show);
        let _ = stdout.flush();
        original(info);
    }));
}

/// Spawn a blocking OS thread that polls crossterm and forwards events
/// onto the async runtime via an unbounded mpsc.
pub fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("gtop-input".into())
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

/// Event-driven render loop. Wakes on a bus event, an input event, or
/// the heartbeat tick — then redraws only if `App` is marked dirty. Idle
/// terminals (no metrics flowing, no keystrokes) draw exactly one frame
/// per `HEARTBEAT_INTERVAL`, not 60 per second.
pub async fn run(
    term: &mut Term,
    app: Arc<Mutex<App>>,
    bus: DataBus,
    mut input_rx: mpsc::UnboundedReceiver<Event>,
) -> io::Result<()> {
    let mut bus_rx = bus.subscribe();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Used to detect a sleep / suspend gap: when consecutive heartbeats
    // arrive >2× the configured interval apart, the host was paused (lid
    // closed, system sleep, et's SSH connection paused). On resume the
    // terminal cells may have been wiped while ratatui's diff buffer
    // thinks they still match, so we trigger one full clear+repaint to
    // resync. Cheaper than a periodic backstop because it only fires
    // when an actual disruption is detected — no idle-time flicker.
    let mut last_heartbeat_at = std::time::Instant::now();

    // Paint the initial frame so users see the layout immediately rather
    // than a blank alt-screen until the first sample arrives.
    {
        let mut g = lock(&app);
        g.take_dirty();
        term.draw(|f| ui::draw(f, &g))?;
    }

    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                tracing::info!("ctrl-c received, shutting down");
                return Ok(());
            }
            recv = bus_rx.recv() => {
                match recv {
                    Ok(ev) => {
                        let mut g = lock(&app);
                        g.apply_event(ev);
                        // Drain any other events that piled up while we were
                        // parked, so a burst of samples turns into one frame
                        // not N frames.
                        drain_bus(&mut bus_rx, &mut g);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "bus receiver lagged");
                        continue;
                    }
                }
            }
            input = input_rx.recv() => {
                match input {
                    Some(ev) => {
                        let flow = {
                            let mut g = lock(&app);
                            g.handle_input(ev)
                        };
                        if matches!(flow, ControlFlow::Quit) {
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                }
            }
            _ = heartbeat.tick() => {
                // Sleep/suspend detection: a heartbeat that's much later
                // than expected means the host was paused. On resume,
                // request a full clear+repaint to resync ratatui's diff
                // buffer with whatever the terminal actually shows now.
                let now = std::time::Instant::now();
                let gap = now.duration_since(last_heartbeat_at);
                last_heartbeat_at = now;
                let mut g = lock(&app);
                if g.boxes.is_enabled(BoxKind::Cpu) {
                    g.mark_dirty();
                }
                if gap > HEARTBEAT_INTERVAL.saturating_mul(3) {
                    g.request_full_repaint();
                }
            }
        }

        // One redraw per wake, gated on dirty so idle ticks are free.
        let mut g = lock(&app);
        let force_full = g.take_force_full_repaint();
        if g.take_dirty() {
            // Resize / FocusGained / periodic-backstop branches set
            // force_full_repaint. `Terminal::clear` wipes the screen AND
            // invalidates ratatui's internal `prev` buffer so the next
            // draw paints every cell (otherwise a SSH/et reconnect leaves
            // stale chrome — actual cells got wiped during the pause but
            // ratatui still thinks they match `prev` and only diffs).
            if force_full {
                term.clear()?;
            }
            term.draw(|f| ui::draw(f, &g))?;
        }
    }
}

fn drain_bus(
    rx: &mut tokio::sync::broadcast::Receiver<crate::core::MetricEvent>,
    app: &mut App,
) {
    loop {
        match rx.try_recv() {
            Ok(ev) => app.apply_event(ev),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "bus receiver lagged");
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
