use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, RunMode};
use crate::model::TestStatus;

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.size();
    let (top_area, main_area, detail_area) = layout_regions(area, app.detail_open);

    draw_top_bar(frame, app, top_area);

    match app.mode {
        RunMode::Selecting => draw_select_list(frame, app, main_area),
        _ => draw_test_list(frame, app, main_area),
    }

    if app.detail_open {
        draw_detail(frame, app, detail_area);
    }
}

fn layout_regions(area: Rect, detail_open: bool) -> (Rect, Rect, Rect) {
    let constraints = if detail_open {
        vec![Constraint::Length(4), Constraint::Min(5), Constraint::Length(30)]
    } else {
        vec![Constraint::Length(4), Constraint::Min(5), Constraint::Length(0)]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    (chunks[0], chunks[1], chunks[2])
}

fn draw_top_bar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mode = match app.mode {
        RunMode::All => "all",
        RunMode::Failing => "failing",
        RunMode::Selected => "selected",
        RunMode::Selecting => "select",
    };
    let (done, total) = app.test_progress();
    let progress = if app.run_state.running {
        format!("running | tests {}/{}", done, total)
    } else {
        format!("idle | tests {}/{}", done, total)
    };
    let line1 = Line::from(vec![
        Span::styled("gest", Style::default().fg(Color::Cyan)),
        Span::raw(" | mode: "),
        Span::raw(mode),
        Span::raw(" | "),
        Span::raw(progress),
    ]);

    let (line2, line3) = match app.mode {
        RunMode::All => (
            "keys: a all, o failing, p select, r rerun, R no-cache",
            "keys: enter toggle output, left close, right open, up/down move, q or ctrl+c quit",
        ),
        RunMode::Failing | RunMode::Selected => (
            "keys: a all, o failing, p select, r rerun, R no-cache, x remove",
            "keys: enter toggle output, left close, right open, up/down move, q or ctrl+c quit",
        ),
        RunMode::Selecting => (
            "keys: type filter, enter/space toggle, p or esc done",
            "keys: a all, o failing, up/down move, ctrl+c quit",
        ),
    };
    let mut lines = vec![line1, Line::from(vec![Span::raw(line2)]), Line::from(vec![Span::raw(line3)])];
    if let Some(error) = app.last_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("error: ", Style::default().fg(Color::Red)),
            Span::raw(error),
        ]));
    }

    let block = Block::default().borders(Borders::ALL).title("status");
    let paragraph = Paragraph::new(Text::from(lines)).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_test_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let tests = app.visible_tests();
    let items: Vec<ListItem> = tests
        .iter()
        .map(|test| {
            let case = app.registry.case(test);
            let status = case.map(|case| case.status).unwrap_or(TestStatus::Unknown);
            let (label, color) = status_label(status);
            let spans = vec![
                Span::styled(format!("{:4}", label), Style::default().fg(color)),
                Span::raw(" "),
                Span::raw(test.name.clone()),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("tests"))
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut list_state = app.list_state.clone();
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_select_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let tests = app.selection.filtered.clone();
    let items: Vec<ListItem> = tests
        .iter()
        .map(|test| {
            let selected = app.selected_set.contains(test);
            let marker = if selected { "[x]" } else { "[ ]" };
            let line = Line::from(vec![Span::raw(format!("{} {}", marker, test.name))]);
            ListItem::new(line)
        })
        .collect();

    let title = format!("select: {}", app.selection.query);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().bg(Color::DarkGray));
    let mut list_state = app.list_state.clone();
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_detail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let content = if let Some(test) = app.current_test() {
        if let Some(case) = app.registry.case(&test) {
            let mut output = String::new();
            if case.panic {
                output.push_str("PANIC DETECTED\n");
            }
            output.push_str(&case.output);
            if output.is_empty() {
                output = "(no output)".to_string();
            }
            output
        } else {
            "(no output)".to_string()
        }
    } else {
        "(no test selected)".to_string()
    };

    frame.render_widget(Clear, area);
    let paragraph = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title("output"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn status_label(status: TestStatus) -> (&'static str, Color) {
    match status {
        TestStatus::Passed => ("PASS", Color::Green),
        TestStatus::Running => ("RUN", Color::Yellow),
        TestStatus::Failed => ("FAIL", Color::Red),
        TestStatus::Unknown => ("----", Color::DarkGray),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;

    use crate::app::{RunState, SelectionState};
    use crate::model::{TestId, TestRegistry};
    use crate::repo::PackageInfo;
    use ratatui::widgets::ListState;

    #[test]
    fn renders_basic_list() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut registry = TestRegistry::default();
        let test = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        registry.ensure_test(&test);
        let app = App {
            mode: RunMode::All,
            registry,
            failing_set: HashSet::new(),
            selected_set: HashSet::new(),
            list_state: ListState::default(),
            detail_open: false,
            selection: SelectionState::default(),
            run_state: RunState::default(),
            packages: vec![PackageInfo {
                import_path: "example".to_string(),
                dir: std::path::PathBuf::from("."),
            }],
            package_filter_active: false,
            repo_root: std::path::PathBuf::from("."),
            watch_enabled: false,
            last_error: None,
        };

        terminal
            .draw(|frame| draw(frame, &app))
            .expect("render should succeed");
    }
}
