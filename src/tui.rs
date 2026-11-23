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
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
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
    /// Cursor position in `input` as a char index.
    cursor: usize,
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
            cursor: 0,
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

    fn byte_index(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map_or(self.input.len(), |(i, _)| i)
    }

    /// Clear the edit line (cursor too); history position is left alone.
    fn clear_line(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Replace the edit line, cursor at end (history recall).
    fn set_input(&mut self, s: String) {
        self.cursor = s.chars().count();
        self.input = s;
    }

    fn insert_char(&mut self, ch: char) {
        let idx = self.byte_index(self.cursor);
        self.input.insert(idx, ch);
        self.cursor += 1;
    }

    fn insert_str(&mut self, s: &str) {
        let idx = self.byte_index(self.cursor);
        self.input.insert_str(idx, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let idx = self.byte_index(self.cursor - 1);
            self.input.remove(idx);
            self.cursor -= 1;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.input.chars().count() {
            let idx = self.byte_index(self.cursor);
            self.input.remove(idx);
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.input.chars().count() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, len) as usize;
    }

    fn word_left(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor = i;
    }

    fn word_right(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor = i;
    }

    /// Ctrl-W: delete the word before the cursor.
    fn word_left_delete(&mut self) {
        let start = {
            let chars: Vec<char> = self.input.chars().collect();
            let mut i = self.cursor;
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[i - 1].is_whitespace() {
                i -= 1;
            }
            i
        };
        let start_byte = self.byte_index(start);
        let end_byte = self.byte_index(self.cursor);
        self.input.replace_range(start_byte..end_byte, "");
        self.cursor = start;
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
                state.insert_str(text);
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
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('c') if ctrl => return Ok(true),
        KeyCode::Char('d') if ctrl && state.input.is_empty() => return Ok(true),
        KeyCode::Char('l') if ctrl => state.log.clear(),
        KeyCode::Char('u') if ctrl => {
            state.clear_line();
            state.hist_idx = None;
        }
        KeyCode::Char('a') if ctrl => state.cursor = 0,
        KeyCode::Char('e') if ctrl => state.cursor = state.input.chars().count(),
        KeyCode::Char('k') if ctrl => {
            let idx = state.byte_index(state.cursor);
            state.input.truncate(idx);
        }
        KeyCode::Char('w') if ctrl => state.word_left_delete(),
        KeyCode::Esc => {
            state.clear_line();
            state.hist_idx = None;
        }
        KeyCode::Char(ch) if !ctrl && !alt => {
            state.insert_char(ch);
        }
        KeyCode::Backspace => state.backspace(),
        KeyCode::Delete => state.delete(),
        KeyCode::Left if ctrl || alt => state.word_left(),
        KeyCode::Right if ctrl || alt => state.word_right(),
        KeyCode::Left => state.move_cursor(-1),
        KeyCode::Right => state.move_cursor(1),
        KeyCode::Home => state.cursor = 0,
        KeyCode::End => state.cursor = state.input.chars().count(),
        KeyCode::Enter => {
            let raw = state.input.trim().to_string();
            state.clear_line();
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
                    let entry = state.history[i].clone();
                    state.set_input(entry);
                    state.hist_idx = Some(i);
                }
            }
        }
        KeyCode::Down => {
            if let Some(i) = state.hist_idx {
                let next = i + 1;
                if next >= state.history.len() {
                    state.hist_idx = None;
                    state.clear_line();
                } else {
                    let entry = state.history[next].clone();
                    state.set_input(entry);
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
        Paragraph::new(Line::from(Span::styled(state.status.clone(), status_style))),
        chunks[2],
    );

    let prompt = "ig> ";
    let prompt_w = prompt.chars().count() as u16;
    let inner_w = chunks[3].width.saturating_sub(prompt_w);
    let chars: Vec<char> = state.input.chars().collect();
    let cursor = state.cursor.min(chars.len());
    let (visible, cursor_x) = if inner_w == 0 {
        (String::new(), chunks[3].right())
    } else {
        // Horizontal scroll so the cursor stays visible on long inputs.
        let offset = if cursor as u16 >= inner_w {
            cursor as u16 - inner_w + 1
        } else {
            0
        };
        let visible: String = chars.iter().skip(offset as usize).collect();
        (visible, chunks[3].x + prompt_w + (cursor as u16 - offset))
    };
    let input_line = Line::from(vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(visible),
    ]);
    frame.render_widget(Paragraph::new(input_line), chunks[3]);
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
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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
        Span::raw(format!("{}   ", output::money(balance.available, &symbol))),
        Span::styled("Balance ", dim),
        Span::raw(format!("{}   ", output::money(balance.balance, &symbol))),
        Span::styled("Margin ", dim),
        Span::raw(format!("{}   ", output::money(balance.deposit, &symbol))),
        Span::styled("P&L ", dim),
        Span::styled(output::signed(balance.profit_loss, &symbol), pnl_style),
    ]);

    vec![title, balance_line]
}

#[cfg(test)]
mod line_edit_tests {
    use super::*;

    fn state_with(input: &str, cursor: usize) -> State {
        let mut s = State::new(false);
        s.input = input.to_string();
        s.cursor = cursor;
        s
    }

    #[test]
    fn insert_char_at_cursor() {
        let mut s = state_with("po", 1);
        s.insert_char('x');
        assert_eq!(s.input, "pxo");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_at_end_and_start() {
        let mut s = state_with("ab", 2);
        s.insert_char('c');
        assert_eq!(s.input, "abc");
        let mut s = state_with("ab", 0);
        s.insert_char('z');
        assert_eq!(s.input, "zab");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn insert_str_multibyte() {
        let mut s = state_with("aé", 1); // cursor between 'a' and 'é'
        s.insert_str("bcd");
        assert_eq!(s.input, "abcdé");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn backspace_deletes_before_cursor() {
        let mut s = state_with("abc", 2);
        s.backspace();
        assert_eq!(s.input, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut s = state_with("abc", 0);
        s.backspace();
        assert_eq!(s.input, "abc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_removes_at_cursor() {
        let mut s = state_with("abc", 1);
        s.delete();
        assert_eq!(s.input, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut s = state_with("abc", 3);
        s.delete();
        assert_eq!(s.input, "abc");
    }

    #[test]
    fn move_cursor_bounds() {
        let mut s = state_with("abc", 0);
        s.move_cursor(-5);
        assert_eq!(s.cursor, 0);
        s.move_cursor(99);
        assert_eq!(s.cursor, 3);
        s.move_cursor(-2);
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn word_motion() {
        let mut s = state_with("order buy VOD", 13);
        s.word_left();
        assert_eq!(s.cursor, 10); // start of VOD
        s.word_left();
        assert_eq!(s.cursor, 6); // start of buy
        s.word_right();
        assert_eq!(s.cursor, 10);
        s.word_right();
        assert_eq!(s.cursor, 13);
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut s = state_with("order buy ", 10);
        s.word_left_delete();
        assert_eq!(s.input, "order ");
        assert_eq!(s.cursor, 6);
        s.word_left_delete();
        assert_eq!(s.input, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn history_recall_puts_cursor_at_end() {
        let mut s = State::new(false);
        s.set_input("balance".into());
        assert_eq!(s.cursor, 7);
        s.clear_line();
        assert_eq!(s.input, "");
        assert_eq!(s.cursor, 0);
    }
}
