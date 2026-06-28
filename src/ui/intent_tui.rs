use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveField {
    What,
    Why,
    Risk,
    Related,
}

#[derive(Debug, Clone)]
pub struct IntentState {
    pub what: String,
    pub why: String,
    pub risk: String, // TRIVIAL, LOW, MEDIUM, HIGH, CRITICAL
    pub related: Vec<String>,
    pub confidence: f64,
    pub active_field: ActiveField,
    pub is_editing: bool,
    pub temp_related: String, // String representation during edit
}

const RISKS: &[&str] = &["TRIVIAL", "LOW", "MEDIUM", "HIGH", "CRITICAL"];

impl IntentState {
    pub fn new(
        what: String,
        why: String,
        risk: String,
        related: Vec<String>,
        confidence: f64,
    ) -> Self {
        let risk_upper = risk.to_uppercase();
        let risk = if RISKS.contains(&risk_upper.as_str()) {
            risk_upper
        } else {
            "MEDIUM".to_string()
        };
        let temp_related = related.join(", ");
        Self {
            what,
            why,
            risk,
            related,
            confidence,
            active_field: ActiveField::What,
            is_editing: false,
            temp_related,
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.what.trim().is_empty() && !self.why.trim().is_empty()
    }
}

pub fn run_tui(mut state: IntentState) -> io::Result<Option<IntentState>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut IntentState,
) -> io::Result<Option<IntentState>> {
    loop {
        terminal
            .draw(|f| draw_ui(f, state))
            .map_err(|e| io::Error::other(e.to_string()))?;

        if let Event::Key(key) = event::read()? {
            if key.kind == event::KeyEventKind::Release {
                continue;
            }
            if state.is_editing {
                handle_editing_key(key, state);
            } else {
                match handle_navigation_key(key, state) {
                    Some(true) => return Ok(Some(state.clone())),
                    Some(false) => return Ok(None),
                    None => {}
                }
            }
        }
    }
}

fn draw_ui(f: &mut ratatui::Frame, state: &IntentState) {
    let size = f.area();

    // Enforce 80x24 standard bounds constraints for the main container
    let area = if size.width > 80 || size.height > 24 {
        let width = size.width.min(80);
        let height = size.height.min(24);
        let x = (size.width - width) / 2;
        let y = (size.height - height) / 2;
        Rect::new(x, y, width, height)
    } else {
        size
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title/Confidence Bar
            Constraint::Length(5), // WHAT (3 lines content + 2 border)
            Constraint::Length(5), // WHY (3 lines content + 2 border)
            Constraint::Length(3), // RISK (1 line content + 2 border)
            Constraint::Length(3), // RELATED (1 line content + 2 border)
            Constraint::Length(5), // Verification/Feedback info
            Constraint::Length(2), // Status Bar (Bottom)
        ])
        .split(area);

    // 1. Title & Confidence Bar
    let conf_color = if state.confidence >= 0.85 {
        Color::Green
    } else {
        Color::Yellow
    };
    let title_line = Line::from(vec![
        Span::styled(
            " Ledgerful Intent Review ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        ),
        Span::raw(" | Confidence: "),
        Span::styled(
            format!("{:.0}%", state.confidence * 100.0),
            Style::default().fg(conf_color).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(title_line), chunks[0]);

    // Borders colors based on active field / validation state
    let get_border_style = |field: ActiveField, val: &str| -> Style {
        if state.active_field == field {
            if state.is_editing {
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            }
        } else if val.trim().is_empty() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };

    // 2. WHAT Block
    let what_border = get_border_style(ActiveField::What, &state.what);
    let what_para = Paragraph::new(state.what.as_str())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" WHAT: Summary of Changes (Required) ")
                .border_style(what_border),
        );
    f.render_widget(what_para, chunks[1]);

    // 3. WHY Block
    let why_border = get_border_style(ActiveField::Why, &state.why);
    let why_para = Paragraph::new(state.why.as_str())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" WHY: Architecture Decision & Rationale (Required) ")
                .border_style(why_border),
        );
    f.render_widget(why_para, chunks[2]);

    // 4. RISK Block
    let risk_border = if state.active_field == ActiveField::Risk {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Format risk choices
    let risk_spans: Vec<Span> = RISKS
        .iter()
        .map(|r| {
            if *r == state.risk {
                let risk_color = match *r {
                    "TRIVIAL" => Color::Gray,
                    "LOW" => Color::Green,
                    "MEDIUM" => Color::Yellow,
                    "HIGH" => Color::LightRed,
                    "CRITICAL" => Color::Red,
                    _ => Color::White,
                };
                Span::styled(
                    format!(" [{}] ", r),
                    Style::default().fg(risk_color).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!("  {}  ", r), Style::default().fg(Color::DarkGray))
            }
        })
        .collect();
    let risk_para = Paragraph::new(Line::from(risk_spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" RISK LEVEL (Use Left/Right arrows) ")
            .border_style(risk_border),
    );
    f.render_widget(risk_para, chunks[3]);

    // 5. RELATED Block
    let related_border = get_border_style(ActiveField::Related, "ok");
    let related_text = if state.is_editing && state.active_field == ActiveField::Related {
        state.temp_related.clone()
    } else {
        state.related.join(", ")
    };

    let related_block_title = if state.is_editing && state.active_field == ActiveField::Related {
        " RELATED TICKETS / ADRS (Comma separated, editing) "
    } else {
        " RELATED TICKETS / ADRS (Chips) "
    };

    let related_para = Paragraph::new(related_text)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(related_block_title)
                .border_style(related_border),
        );
    f.render_widget(related_para, chunks[4]);

    // 6. Verification/Feedback Info
    let mut validation_msg = Vec::new();
    if state.what.trim().is_empty() {
        validation_msg.push(Span::styled(
            "● WHAT field is empty. ",
            Style::default().fg(Color::Red),
        ));
    }
    if state.why.trim().is_empty() {
        validation_msg.push(Span::styled(
            "● WHY field is empty. ",
            Style::default().fg(Color::Red),
        ));
    }
    if state.is_valid() {
        validation_msg.push(Span::styled(
            "✓ Intent validation passed. Cryptographic transaction ready to sign.",
            Style::default().fg(Color::Green),
        ));
    }

    let feedback_text = vec![
        Line::from(validation_msg),
        Line::from(""),
        Line::from(vec![
            Span::styled("Provenance target: ", Style::default().fg(Color::Gray)),
            Span::styled("Ledgerful Ledger", Style::default().fg(Color::White)),
        ]),
    ];
    let feedback_para = Paragraph::new(feedback_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" System Status & Validation ")
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(feedback_para, chunks[5]);

    // 7. Status Bar
    let edit_state_str = if state.is_editing {
        "[EDIT MODE: Type to edit. Press Esc/Enter to finish editing]"
    } else {
        "[Tab/Arrows: Navigate | e: Edit Active Field | Enter: Accept Commit | s: Skip | Esc: Abort]"
    };
    let status_para = Paragraph::new(edit_state_str)
        .style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .block(Block::default());
    f.render_widget(status_para, chunks[6]);
}

fn handle_editing_key(key: KeyEvent, state: &mut IntentState) {
    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            state.is_editing = false;
            // Parse related if finishing edit on it
            if state.active_field == ActiveField::Related {
                state.related = state
                    .temp_related
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                state.temp_related = state.related.join(", ");
            }
        }
        KeyCode::Char(c) => match state.active_field {
            ActiveField::What => state.what.push(c),
            ActiveField::Why => state.why.push(c),
            ActiveField::Related => state.temp_related.push(c),
            _ => {}
        },
        KeyCode::Backspace => match state.active_field {
            ActiveField::What => {
                state.what.pop();
            }
            ActiveField::Why => {
                state.why.pop();
            }
            ActiveField::Related => {
                state.temp_related.pop();
            }
            _ => {}
        },
        _ => {}
    }
}

fn handle_navigation_key(key: KeyEvent, state: &mut IntentState) -> Option<bool> {
    match key.code {
        KeyCode::Esc => {
            // Abort
            return Some(false);
        }
        KeyCode::Char('s') if !state.is_editing => {
            // Skip
            state.risk = "TRIVIAL".to_string();
            state.what = "Skipped intent entry".to_string();
            state.why = "Trivial change bypass".to_string();
            return Some(true);
        }
        KeyCode::Enter if state.is_valid() => {
            return Some(true);
        }
        KeyCode::Tab => {
            // Cycle fields
            state.active_field = match state.active_field {
                ActiveField::What => ActiveField::Why,
                ActiveField::Why => ActiveField::Risk,
                ActiveField::Risk => ActiveField::Related,
                ActiveField::Related => ActiveField::What,
            };
        }
        KeyCode::BackTab => {
            // Cycle fields backwards
            state.active_field = match state.active_field {
                ActiveField::What => ActiveField::Related,
                ActiveField::Why => ActiveField::What,
                ActiveField::Risk => ActiveField::Why,
                ActiveField::Related => ActiveField::Risk,
            };
        }
        KeyCode::Char('e') if state.active_field != ActiveField::Risk => {
            state.is_editing = true;
            if state.active_field == ActiveField::Related {
                state.temp_related = state.related.join(", ");
            }
        }
        KeyCode::Right => {
            if state.active_field == ActiveField::Risk {
                let idx = RISKS.iter().position(|r| *r == state.risk).unwrap_or(2);
                let next_idx = (idx + 1) % RISKS.len();
                state.risk = RISKS[next_idx].to_string();
            } else {
                // Also support tab-like behavior with down/right arrows
                state.active_field = match state.active_field {
                    ActiveField::What => ActiveField::Why,
                    ActiveField::Why => ActiveField::Risk,
                    ActiveField::Risk => ActiveField::Related,
                    ActiveField::Related => ActiveField::What,
                };
            }
        }
        KeyCode::Left => {
            if state.active_field == ActiveField::Risk {
                let idx = RISKS.iter().position(|r| *r == state.risk).unwrap_or(2);
                let prev_idx = (idx + RISKS.len() - 1) % RISKS.len();
                state.risk = RISKS[prev_idx].to_string();
            } else {
                state.active_field = match state.active_field {
                    ActiveField::What => ActiveField::Related,
                    ActiveField::Why => ActiveField::What,
                    ActiveField::Risk => ActiveField::Why,
                    ActiveField::Related => ActiveField::Risk,
                };
            }
        }
        KeyCode::Up => {
            state.active_field = match state.active_field {
                ActiveField::What => ActiveField::Related,
                ActiveField::Why => ActiveField::What,
                ActiveField::Risk => ActiveField::Why,
                ActiveField::Related => ActiveField::Risk,
            };
        }
        KeyCode::Down => {
            state.active_field = match state.active_field {
                ActiveField::What => ActiveField::Why,
                ActiveField::Why => ActiveField::Risk,
                ActiveField::Risk => ActiveField::Related,
                ActiveField::Related => ActiveField::What,
            };
        }
        _ => {}
    }
    None
}
