//! The workflow strip under the usage strip: for a team whose style has a
//! workflow, the phase the team is on and where each of its steps stands,
//! read off the cards (`crew_core::workflow`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::app::App;
use crate::theme::Theme;
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, width};

const LABEL: &str = "workflow ";

/// The strip's text, when there is one: the headline of the phase the
/// team is on, or the steps while none has been laid out.
pub fn text(app: &App) -> Option<String> {
    let workflow = app.session.workflow.as_ref()?;
    Some(match workflow.headline(&app.tasks) {
        Some(headline) => headline,
        None => format!(
            "{} · no phase laid out yet",
            workflow.step_ids().join(" → ")
        ),
    })
}

/// How many rows the strip takes on a screen of `area`: one, when the
/// team has a workflow and the screen has room for it.
pub fn height(app: &App, area: Rect) -> u16 {
    if area.height < 12 || area.width < 40 {
        return 0;
    }
    u16::from(app.session.workflow.is_some())
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let Some(text) = text(app) else { return };
    let bar = Style::default().bg(theme.bar_bg);
    fill_row(buf, area, area.y, bar);
    let x = i32::from(area.x) + 1;
    let y = i32::from(area.y);
    let w = usize::from(area.width).saturating_sub(2);
    put_str(buf, area, x, y, LABEL, bar.fg(theme.muted));
    put_str(
        buf,
        area,
        x + width(LABEL) as i32,
        y,
        &fit(&text, w.saturating_sub(width(LABEL))),
        bar.fg(theme.text),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::workflow::{StepInfo, WorkflowInfo, WorkflowPhase};
    use crew_core::{Task, TaskStatus};

    #[test]
    fn the_strip_shows_the_phase_the_team_is_on_and_takes_a_row_only_with_a_workflow() {
        let mut app = App::new();
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(height(&app, area), 0);
        assert!(text(&app).is_none());
        app.session.workflow = Some(WorkflowInfo {
            steps: ["research", "plan"]
                .into_iter()
                .map(|id| StepInfo {
                    id: id.into(),
                    slice: false,
                })
                .collect(),
            phases: Vec::new(),
        });
        assert_eq!(height(&app, area), 1);
        assert_eq!(
            text(&app).as_deref(),
            Some("research → plan · no phase laid out yet")
        );
        app.session
            .workflow
            .as_mut()
            .unwrap()
            .phases
            .push(WorkflowPhase {
                phase: "03-auth".into(),
                title: "Authentication".into(),
                laid_at: 0,
                brief: String::new(),
                cards: [("research", "research-03-auth"), ("plan", "plan-03-auth")]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                ..WorkflowPhase::default()
            });
        let task = |id: &str, status: TaskStatus| -> Task {
            serde_json::from_value(serde_json::json!({
                "id": id, "title": id, "description": "", "status": status,
                "assigned_agent": "x", "created_at": 0
            }))
            .unwrap()
        };
        app.tasks = vec![
            task("research-03-auth", TaskStatus::Done),
            task("plan-03-auth", TaskStatus::Working),
        ];
        assert_eq!(
            text(&app).as_deref(),
            Some("phase 1/1 · 03-auth (Authentication): research ✓ · plan ● x")
        );
        // A short screen keeps its rows for the agents.
        assert_eq!(height(&app, Rect::new(0, 0, 100, 10)), 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 1));
        render(&mut buf, Rect::new(0, 0, 60, 1), &app, &Theme::truecolor());
        let row: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.starts_with(" workflow phase 1/1 · 03-auth"), "{row:?}");
    }
}
