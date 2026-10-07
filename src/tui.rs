//! Terminal UI for the microphone.
//!
//! The receiver streams its level meter and announces every physical button
//! press, so the interface follows the hardware rather than polling it. Only
//! battery is polled, since it is the one value that moves slowly.
//!
//! Everything is reachable three ways: mouse (click, drag, scroll), keyboard
//! shortcuts, and Tab focus with arrow keys.

use maono::mic::{
    level_fraction, Mic, BATTERY, GAIN, GAIN_MAX, LEVEL, LIGHT, LIGHT_MODE, LIGHT_MODE_MAX,
    MUTE, NR, NR_LEVEL, NR_NAMES,
};
use crate::theme;
use crate::widgets::{bar_graph, keycap, led_meter, meter_scale, slider};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

const SEGMENTS: [&str; 4] = ["off", "low", "mid", "high"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Mute,
    Gain,
    Noise,
    Light,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Mute => Focus::Gain,
            Focus::Gain => Focus::Noise,
            Focus::Noise => Focus::Light,
            Focus::Light => Focus::Mute,
        }
    }
    fn prev(self) -> Self {
        self.next().next().next()
    }
}

/// Click targets, refreshed from the layout every frame so hit-testing can
/// never drift away from what is actually on screen.
#[derive(Default, Clone, Copy)]
struct Hits {
    mute: Rect,
    gain: Rect,
    segments: [Rect; 4],
    light_toggle: Rect,
    light_prev: Rect,
    light_next: Rect,
    close: Rect,
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

struct App {
    mic: Mic,
    battery: Option<u16>,
    muted: bool,
    gain: u16,
    nr_on: bool,
    nr_level: u16,
    light_on: bool,
    light_mode: u16,
    level: f64,
    peak: f64,
    peak_at: Instant,
    clip_at: Option<Instant>,
    history: VecDeque<u64>,
    status: String,
    focus: Focus,
    hits: Hits,
    mouse: Option<(u16, u16)>,
    dragging: bool,
    quit: bool,
}

impl App {
    fn new(mut mic: Mic) -> io::Result<Self> {
        Ok(Self {
            battery: mic.get(BATTERY)?,
            muted: mic.get(MUTE)?.unwrap_or(0) != 0,
            gain: mic.get(GAIN)?.unwrap_or(0),
            nr_on: mic.get(NR)?.unwrap_or(0) != 0,
            nr_level: mic.get(NR_LEVEL)?.unwrap_or(0),
            light_on: mic.get(LIGHT)?.unwrap_or(0) != 0,
            light_mode: mic.get(LIGHT_MODE)?.unwrap_or(0),
            mic,
            level: 0.0,
            peak: 0.0,
            peak_at: Instant::now(),
            clip_at: None,
            history: VecDeque::with_capacity(600),
            status: "connected".into(),
            focus: Focus::Mute,
            hits: Hits::default(),
            mouse: None,
            dragging: false,
            quit: false,
        })
    }

    /// Take in whatever the device has announced since the last frame.
    fn absorb(&mut self) {
        for (id, val) in self.mic.drain() {
            match id {
                LEVEL => {
                    self.level = level_fraction(val);
                    if self.level >= 0.95 {
                        self.clip_at = Some(Instant::now());
                    }
                    if self.level >= self.peak {
                        self.peak = self.level;
                        self.peak_at = Instant::now();
                    }
                }
                MUTE => self.muted = val != 0,
                GAIN => self.gain = val,
                NR => self.nr_on = val != 0,
                NR_LEVEL => self.nr_level = val,
                LIGHT => self.light_on = val != 0,
                LIGHT_MODE => self.light_mode = val,
                BATTERY => self.battery = Some(val),
                _ => {}
            }
        }
        // hold the peak briefly, then let it fall back to the current level
        if self.peak_at.elapsed() > Duration::from_millis(1200) {
            self.peak = (self.peak - 0.012).max(self.level);
        }
    }

    /// Sample on a fixed cadence so the activity graph scrolls evenly,
    /// regardless of how often the device happens to report.
    fn record(&mut self) {
        let v = if self.muted { 0.0 } else { self.level };
        self.history.push_back((v * 100.0) as u64);
        while self.history.len() > 600 {
            self.history.pop_front();
        }
    }

    fn clipping(&self) -> bool {
        self.clip_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
    }

    fn set_mute(&mut self, want: bool) {
        if self.mic.set(MUTE, want as u16).is_ok() {
            self.muted = want;
            self.status = if want { "muted" } else { "live" }.into();
        }
    }

    fn set_gain(&mut self, want: u16) {
        let want = want.min(GAIN_MAX);
        if want != self.gain && self.mic.set(GAIN, want).is_ok() {
            self.gain = want;
            self.status = format!("gain {want}");
        }
    }

    fn nudge_gain(&mut self, d: i32) {
        self.set_gain((self.gain as i32 + d).clamp(0, GAIN_MAX as i32) as u16);
    }

    /// 0 = off, 1..=3 = low/mid/high, matching the receiver's own button order.
    fn set_nr(&mut self, index: usize) {
        let on = index > 0;
        let level = index.saturating_sub(1) as u16;
        let _ = self.mic.set(NR, on as u16);
        std::thread::sleep(Duration::from_millis(110));
        let _ = self.mic.set(NR_LEVEL, level);
        self.nr_on = on;
        self.nr_level = level;
        self.status = if on {
            format!("noise {}", NR_NAMES[level as usize])
        } else {
            "noise off".into()
        };
    }

    fn set_light(&mut self, on: bool) {
        if self.mic.set(LIGHT, on as u16).is_ok() {
            self.light_on = on;
            self.status = if on { "light on" } else { "light off" }.into();
        }
    }

    /// Step through the light modes the way the receiver's own button does.
    fn step_light_mode(&mut self, delta: i32) {
        let span = LIGHT_MODE_MAX as i32 + 1;
        let next = ((self.light_mode as i32 + delta) % span + span) % span;
        let _ = self.mic.set(LIGHT, 1);
        std::thread::sleep(Duration::from_millis(90));
        if self.mic.set(LIGHT_MODE, next as u16).is_ok() {
            self.light_on = true;
            self.light_mode = next as u16;
            self.status = format!("light mode {next}");
        }
    }

    fn nr_index(&self) -> usize {
        if self.nr_on {
            (self.nr_level as usize + 1).min(3)
        } else {
            0
        }
    }

    fn gain_from_x(&self, x: u16) -> u16 {
        let t = self.hits.gain;
        if t.width <= 1 {
            return self.gain;
        }
        let rel = x.saturating_sub(t.x).min(t.width - 1) as f64;
        ((rel / (t.width - 1) as f64) * GAIN_MAX as f64).round() as u16
    }

    fn hovered(&self, r: Rect) -> bool {
        self.mouse.is_some_and(|(x, y)| inside(r, x, y))
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        self.mouse = Some((x, y));
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if inside(self.hits.close, x, y) {
                    self.quit = true;
                } else if inside(self.hits.mute, x, y) {
                    self.focus = Focus::Mute;
                    self.set_mute(!self.muted);
                } else if inside(self.hits.gain, x, y) {
                    self.focus = Focus::Gain;
                    self.dragging = true;
                    let v = self.gain_from_x(x);
                    self.set_gain(v);
                } else if let Some(i) = self.hits.segments.iter().position(|r| inside(*r, x, y)) {
                    self.focus = Focus::Noise;
                    self.set_nr(i);
                } else if inside(self.hits.light_toggle, x, y) {
                    self.focus = Focus::Light;
                    self.set_light(!self.light_on);
                } else if inside(self.hits.light_prev, x, y) {
                    self.focus = Focus::Light;
                    self.step_light_mode(-1);
                } else if inside(self.hits.light_next, x, y) {
                    self.focus = Focus::Light;
                    self.step_light_mode(1);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let v = self.gain_from_x(x);
                self.set_gain(v);
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = false,
            MouseEventKind::ScrollUp => self.nudge_gain(1),
            MouseEventKind::ScrollDown => self.nudge_gain(-1),
            _ => {}
        }
    }

    /// Arrow keys act on whatever has focus, the way a form behaves.
    fn on_arrow(&mut self, forward: bool) {
        match self.focus {
            Focus::Mute => self.set_mute(!self.muted),
            Focus::Gain => self.nudge_gain(if forward { 1 } else { -1 }),
            Focus::Noise => {
                let i = self.nr_index() as i32 + if forward { 1 } else { -1 };
                self.set_nr(i.clamp(0, 3) as usize);
            }
            Focus::Light => self.step_light_mode(if forward { 1 } else { -1 }),
        }
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// Label column width, so every row lines up without any boxes.
const LABEL: u16 = 7;

fn label(text: &str, focused: bool) -> Span<'static> {
    Span::styled(
        format!("{text:<w$}", w = LABEL as usize),
        Style::default().fg(if focused { theme::FOCUS } else { theme::FAINT }),
    )
}

/// Split a row into its label cell and the rest.
fn split(row: Rect) -> (Rect, Rect) {
    let l = Rect {
        width: LABEL.min(row.width),
        ..row
    };
    let r = Rect {
        x: row.x + LABEL.min(row.width),
        width: row.width.saturating_sub(LABEL),
        ..row
    };
    (l, r)
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin::new(1, 0));
    let [head, _s0, meter, scale, _s1, mic, gain, noise, light, _s2, act, status] =
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);

    draw_head(f, app, head);
    draw_meter(f, app, meter, scale);
    draw_mic(f, app, mic);
    draw_gain(f, app, gain);
    draw_noise(f, app, noise);
    draw_light(f, app, light);
    draw_activity(f, app, act);
    draw_status(f, app, status);
}

fn draw_head(f: &mut Frame, app: &mut App, row: Rect) {
    let batt = app.battery.unwrap_or(0);
    let (state, colour) = if app.muted {
        ("MUTED", theme::RED)
    } else {
        ("LIVE", theme::GREEN)
    };
    app.hits.close = Rect {
        x: row.x + row.width.saturating_sub(1),
        width: 1,
        ..row
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "maono pd100w",
                Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("   ⏺ ", Style::default().fg(colour)),
            Span::styled(state, Style::default().fg(colour)),
            Span::styled("   ", Style::default()),
            Span::styled(
                format!("{batt}%"),
                Style::default().fg(theme::battery(batt)),
            ),
        ])),
        row,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "✕",
            Style::default().fg(if app.hovered(app.hits.close) {
                theme::RED
            } else {
                theme::FAINT
            }),
        )))
        .alignment(Alignment::Right),
        row,
    );
}

fn draw_meter(f: &mut Frame, app: &mut App, bar: Rect, scale: Rect) {
    let (l, r) = split(bar);
    // reserve the right edge for the numeric readout
    let readout = 13u16.min(r.width);
    let track = Rect {
        width: r.width.saturating_sub(readout),
        ..r
    };

    f.render_widget(Paragraph::new(Line::from(label("level", false))), l);
    f.render_widget(
        Paragraph::new(led_meter(track.width, app.level, app.peak, app.muted)),
        track,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if app.muted {
                    "  muted".to_string()
                } else {
                    format!("{:>4}%", (app.level * 100.0) as u16)
                },
                Style::default().fg(if app.muted {
                    theme::MUTE_TEXT
                } else {
                    theme::zone(app.level)
                }),
            ),
            if app.clipping() {
                Span::styled(
                    " CLIP",
                    Style::default()
                        .fg(theme::RED)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" pk{:>3}", (app.peak * 100.0) as u16),
                    Style::default().fg(theme::FAINT),
                )
            },
        ]))
        .alignment(Alignment::Right),
        r,
    );

    let (_, sr) = split(scale);
    f.render_widget(
        Paragraph::new(meter_scale(sr.width.saturating_sub(readout))),
        sr,
    );
}

fn draw_mic(f: &mut Frame, app: &mut App, row: Rect) {
    let (l, r) = split(row);
    let focused = app.focus == Focus::Mute;
    let pill = Rect {
        width: 8.min(r.width),
        ..r
    };
    app.hits.mute = pill;
    let hot = app.hovered(pill) || focused;

    f.render_widget(Paragraph::new(Line::from(label("mic", focused))), l);
    let (fill, text) = if app.muted {
        (theme::RED, "  mute  ")
    } else {
        (theme::GREEN, "  live  ")
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                text,
                Style::default()
                    .bg(fill)
                    .fg(theme::INK)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if hot {
                    if app.muted {
                        "  click to unmute"
                    } else {
                        "  click to mute"
                    }
                } else {
                    ""
                },
                Style::default().fg(theme::FAINT),
            ),
        ])),
        r,
    );
}

fn draw_gain(f: &mut Frame, app: &mut App, row: Rect) {
    let (l, r) = split(row);
    let focused = app.focus == Focus::Gain;
    let readout = 8u16.min(r.width);
    let track = Rect {
        width: r.width.saturating_sub(readout),
        ..r
    };
    app.hits.gain = track;
    let hot = app.hovered(track) || app.dragging || focused;

    f.render_widget(Paragraph::new(Line::from(label("gain", focused))), l);
    f.render_widget(
        Paragraph::new(slider(
            track.width,
            app.gain as f64 / GAIN_MAX as f64,
            theme::CYAN,
            hot,
        )),
        track,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{:>2}/{GAIN_MAX}", app.gain),
            Style::default().fg(theme::TEXT),
        )))
        .alignment(Alignment::Right),
        r,
    );
}

fn draw_noise(f: &mut Frame, app: &mut App, row: Rect) {
    let (l, r) = split(row);
    let focused = app.focus == Focus::Noise;
    f.render_widget(Paragraph::new(Line::from(label("noise", focused))), l);

    let selected = app.nr_index();
    let mut x = r.x;
    let mut rects = [Rect::default(); 4];
    for (i, name) in SEGMENTS.iter().enumerate() {
        let w = name.len() as u16 + 2;
        if x + w > r.x + r.width {
            break;
        }
        let cell = Rect {
            x,
            y: r.y,
            width: w,
            height: 1,
        };
        rects[i] = cell;
        let on = i == selected;
        let hot = app.hovered(cell);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {name} "),
                if on {
                    Style::default()
                        .bg(theme::VIOLET)
                        .fg(theme::INK)
                        .add_modifier(Modifier::BOLD)
                } else if hot {
                    Style::default().bg(theme::LINE).fg(theme::TEXT)
                } else {
                    Style::default().fg(theme::MUTE_TEXT)
                },
            ))),
            cell,
        );
        x += w;
    }
    app.hits.segments = rects;
}

fn draw_light(f: &mut Frame, app: &mut App, row: Rect) {
    let (l, r) = split(row);
    let focused = app.focus == Focus::Light;
    f.render_widget(Paragraph::new(Line::from(label("light", focused))), l);

    let [toggle, prev, num, next, _] = Layout::horizontal([
        Constraint::Length(5),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .areas(r);
    app.hits.light_toggle = toggle;
    app.hits.light_prev = prev;
    app.hits.light_next = next;

    let on = app.light_on;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if on { " on  " } else { " off " },
            if on {
                Style::default()
                    .bg(theme::AMBER)
                    .fg(theme::INK)
                    .add_modifier(Modifier::BOLD)
            } else if app.hovered(toggle) {
                Style::default().bg(theme::LINE).fg(theme::TEXT)
            } else {
                Style::default().fg(theme::MUTE_TEXT)
            },
        ))),
        toggle,
    );
    for (rect, glyph) in [(prev, "◀"), (next, "▶")] {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                glyph,
                Style::default().fg(if app.hovered(rect) {
                    theme::TEXT
                } else {
                    theme::MUTE_TEXT
                }),
            )))
            .alignment(Alignment::Center),
            rect,
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.light_mode.to_string(),
            Style::default().fg(if on { theme::AMBER } else { theme::FAINT }),
        )))
        .alignment(Alignment::Center),
        num,
    );
}

fn draw_activity(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let (_, r) = split(area);
    let data: Vec<u64> = app.history.iter().copied().collect();
    f.render_widget(
        Paragraph::new(bar_graph(r.width, area.height, &data, app.muted)),
        r,
    );
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    let mut spans = Vec::new();
    for (k, v) in [
        ("tab", "focus"),
        ("←/→", "adjust"),
        ("m", "mute"),
        ("l", "light"),
        ("q", "quit"),
    ] {
        spans.extend(keycap(k, v));
    }
    spans.push(Span::styled(&app.status, Style::default().fg(theme::CYAN)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn run() -> io::Result<()> {
    let lock = std::fs::File::create(maono::store::lock_path())?;
    if lock.try_lock().is_err() {
        return Err(std::io::Error::other("`maono serve` owns the mic (the Omarchy panel); use the panel or stop serve first"));
    }
    let mut app = App::new(Mic::open()?)?;
    let mut term = ratatui::init();
    execute!(io::stdout(), EnableMouseCapture)?;
    let mut last_battery = Instant::now();

    let result = (|| -> io::Result<()> {
        while !app.quit {
            app.absorb();
            app.record();
            term.draw(|f| draw(f, &mut app))?;

            if last_battery.elapsed() > Duration::from_secs(30) {
                if let Ok(Some(b)) = app.mic.get(BATTERY) {
                    app.battery = Some(b);
                }
                last_battery = Instant::now();
            }

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.quit = true
                        }
                        KeyCode::Tab => app.focus = app.focus.next(),
                        KeyCode::BackTab => app.focus = app.focus.prev(),
                        KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => {
                            app.on_arrow(true)
                        }
                        KeyCode::Left | KeyCode::Char('-') => app.on_arrow(false),
                        KeyCode::Enter | KeyCode::Char(' ') => app.on_arrow(true),
                        KeyCode::Char('m') => app.set_mute(!app.muted),
                        KeyCode::Char('l') => app.set_light(!app.light_on),
                        KeyCode::Home => app.set_gain(0),
                        KeyCode::End => app.set_gain(GAIN_MAX),
                        KeyCode::Char(c @ '0'..='3') => {
                            app.set_nr(c.to_digit(10).unwrap() as usize)
                        }
                        _ => {}
                    },
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
