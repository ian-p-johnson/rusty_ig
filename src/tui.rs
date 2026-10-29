use crate::client::{IgClient, Session};
use crate::names::NameCache;
use crate::ops::{self, Cmd};
use crate::output;
use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};
use std::io;
use tokio::sync::mpsc;

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

struct State {
    input: String,
    history: Vec<String>,
    hist_idx: Option<usize>,
    scroll: u16,
    viewport: u16,
    log: Vec<Line<'static>>,
    status: String,
    busy: bool,
    live: bool,
}

impl State {
    fn new(live: bool) -> Self {
        Self {
            input: String::new(),
            history: Vec::new(),
            hist_idx: None,
            scroll: 0,
            viewport: 0,
            log: Vec::new(),
            status: "ready".into(),
            busy: false,
            live,
        }
    }

    fn push_block(&mut self, block: &str) {
        for line in block.lines() {
            self.log.push(Line::from(line.to_string()));
        }
        self.scroll = u16::MAX;
    }

    fn push_echo(&mut self, cmd: &str) {
        self.log.push(Line::from(Span::styled(
            format!("> {cmd}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        self.scroll = u16::MAX;
    }

    fn push_error(&mut self, msg: &str) {
        self.log.push(Line::from(Span::styled(
            msg.to_string(),
            Style::default().fg(Color::Red),
        )));
        self.scroll = u16::MAX;
    }

    fn push_info(&mut self, msg: &str) {
        self.log.push(Line::from(Span::styled(
            msg.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        self.scroll = u16::MAX;
    }

    fn max_offset(&self) -> u16 {
        self.log.len().saturating_sub(self.viewport as usize) as u16
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_offset();
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

pub async fn run(
    api: &mut IgClient,
    session: &mut Session,
    cache: &mut NameCache,
    live: bool,
    identifier: &str,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = event_loop(&mut terminal, api, session, cache, live, identifier).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    let _ = terminal.show_cursor();

    match api.logout().await {
        Ok(()) => println!("Logged out."),
        Err(err) => eprintln!("warning: logout failed: {err:#}"),
    }
    result
}

async fn event_loop(
    terminal: &mut Tui,
    api: &IgClient,
    session: &mut Session,
    cache: &mut NameCache,
    live: bool,
    identifier: &str,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(ev) => {
                if tx.send(ev).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });

    let mut state = State::new(live);
    state.push_info(&format!(
        "Logged in to {} as {}",
        if live { "LIVE" } else { "DEMO" },
        identifier
    ));
    state.push_info("Type `help` for commands.");
    state.push_block("");

    loop {
        terminal.draw(|f| draw(f, &mut state, session))?;
        let Some(ev) = rx.recv().await else { break };
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(&mut state, terminal, api, session, cache, key).await? {
                    break;
                }
            }
            Event::Paste(text) => {
                let text = text.trim_end_matches(['\r', '\n']);
                state.input.push_str(text);
            }
            _ => {}
        }
    }
    Ok(())
}

async fn handle_key(
    state: &mut State,
    terminal: &mut Tui,
    api: &IgClient,
    session: &mut Session,
    cache: &mut NameCache,
    key: KeyEvent,
) -> Result<bool> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => return Ok(true),
        KeyCode::Char('d') if ctrl && state.input.is_empty() => return Ok(true),
        KeyCode::Char('l') if ctrl => state.log.clear(),
        KeyCode::Char('u') if ctrl => state.input.clear(),
        KeyCode::Esc => state.input.clear(),
        KeyCode::Char(ch) if !ctrl => {
            state.input.push(ch);
            state.hist_idx = None;
        }
        KeyCode::Backspace => {
            state.input.pop();
            state.hist_idx = None;
        }
        KeyCode::Enter => {
            let raw = state.input.trim().to_string();
            state.input.clear();
            state.hist_idx = None;
            if raw.is_empty() {
                return Ok(false);
            }
            state.push_echo(&raw);
            if state.history.last().map(String::as_str) != Some(raw.as_str()) {
                state.history.push(raw.clone());
            }
            let cmds = match ops::parse_input(&raw) {
                Ok(cmds) => cmds,
                Err(msg) => {
                    state.push_error(&msg);
                    return Ok(false);
                }
            };
            state.status = format!("running `{raw}`...");
            state.busy = true;
            draw_snapshot(terminal, state, session)?;
            let width = terminal.size()?.width.saturating_sub(4);
            let mut quit = false;
            for cmd in cmds {
                if matches!(cmd, Cmd::Quit) {
                    quit = true;
                    break;
                }
                if matches!(cmd, Cmd::Clear) {
                    state.log.clear();
                    continue;
                }
                match ops::exec(cmd, api, session, cache, Some(width)).await {
                    Ok(out) => {
                        if !out.is_empty() {
                            state.push_block(&out);
                        }
                    }
                    Err(err) => state.push_error(&format!("{err:#}")),
                }
            }
            state.busy = false;
            if quit {
                state.status = "logging out...".into();
                return Ok(true);
            }
            state.status = "ready".into();
        }
        KeyCode::Up => {
            if !state.history.is_empty() {
                let i = match state.hist_idx {
                    None => state.history.len(),
                    Some(i) => i,
                };
                if i > 0 {
                    let i = i - 1;
                    state.input = state.history[i].clone();
                    state.hist_idx = Some(i);
                }
            }
        }
        KeyCode::Down => {
            if let Some(i) = state.hist_idx {
                let next = i + 1;
                if next >= state.history.len() {
                    state.hist_idx = None;
                    state.input.clear();
                } else {
                    state.input = state.history[next].clone();
                    state.hist_idx = Some(next);
                }
            }
        }
        KeyCode::PageUp => {
            state.scroll = state.scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.scroll = (state.scroll + 10).min(state.max_offset());
        }
        _ => {}
    }
    Ok(false)
}

fn draw_snapshot(terminal: &mut Tui, state: &mut State, session: &Session) -> Result<()> {
    terminal.draw(|f| draw(f, state, session))?;
    Ok(())
}

fn draw(frame: &mut Frame, state: &mut State, session: &Session) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header = header_lines(session, state.live);
    frame.render_widget(
        Paragraph::new(header).block(Block::bordered().title(" account ")),
        chunks[0],
    );

    state.viewport = chunks[1].height.saturating_sub(2);
    state.clamp_scroll();
    let output = Paragraph::new(Text::from(state.log.clone()))
        .block(Block::bordered().title(format!(
            " output{} ",
            if state.scroll > 0 {
                format!("- {}/{}", state.scroll, state.max_offset())
            } else {
                String::new()
            }
        )))
        .scroll((state.scroll, 0));
    frame.render_widget(output, chunks[1]);

    let status_style = if state.busy {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            state.status.clone(),
            status_style,
        ))),
        chunks[2],
    );

    let prompt = "ig> ";
    let input_line = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(state.input.clone()),
        Span::styled("█", Style::default().fg(Color::White)),
    ]);
    frame.render_widget(Paragraph::new(input_line), chunks[3]);
    let cursor_x = chunks[3].x + prompt.chars().count() as u16 + state.input.chars().count() as u16;
    if cursor_x < chunks[3].right() {
        frame.set_cursor_position((cursor_x, chunks[3].y));
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "balance positions orders | search open order cancel modify close | help clear quit | PgUp/PgDn Ctrl-C",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[4],
    );
}

fn header_lines(session: &Session, live: bool) -> Vec<Line<'static>> {
    let env = if live {
        Span::styled(
            "IG LIVE",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "IG DEMO",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let title = Line::from(vec![
        env,
        Span::raw("  "),
        Span::styled(
            output::session_label(session),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {}  ", session.account_id)),
        Span::styled(
            session.currency.clone().unwrap_or_default(),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let balance = session
        .current_account()
        .and_then(|a| a.balance.clone())
        .unwrap_or_default();
    let symbol = session.currency_symbol.clone().unwrap_or_default();
    let pnl_style = match balance.profit_loss {
        Some(v) if v < 0.0 => Style::default().fg(Color::Red),
        Some(_) => Style::default().fg(Color::Green),
        None => Style::default().fg(Color::DarkGray),
    };
    let dim = Style::default().fg(Color::DarkGray);
    let balance_line = Line::from(vec![
        Span::styled("Available ", dim),
        Span::raw(format!(
            "{}   ",
            output::money(balance.available, &symbol)
        )),
        Span::styled("Balance ", dim),
        Span::raw(format!("{}   ", output::money(balance.balance, &symbol))),
        Span::styled("Margin ", dim),
        Span::raw(format!("{}   ", output::money(balance.deposit, &symbol))),
        Span::styled("P&L ", dim),
        Span::styled(output::signed(balance.profit_loss, &symbol), pnl_style),
    ]);

    vec![title, balance_line]
}
