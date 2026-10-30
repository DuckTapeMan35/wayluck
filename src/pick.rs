//! `wayluck pick`: xdg-desktop-portal-wlr's chooser_cmd. When an app asks to
//! share the screen, the portal runs it: it asks wayluck what can be shared,
//! shows the user a picker with live thumbnails, and prints the choice the
//! way the portal wants it ("Monitor: NAME", "Window: ID"). The picker is a window like
//! any other: the window manager places it (fixed size: floating).

mod ask;
mod capture;
mod text;
mod window;

/// Run `wayluck pick`. The exit code: 0 with the choice printed, or 1 if
/// the user cancelled or on an error (said on stderr): the portal shares
/// nothing then.
pub fn run() -> i32 {
    match pick() {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            eprintln!("wayluck pick: {e}");
            1
        }
    }
}

/// One row of the picker: how it's shown, what its thumbnail shows, and
/// what's printed if it's chosen (None: it can't be, yet).
struct Choice {
    row: window::Row,
    source: capture::Source,
    answer: Option<String>,
}

impl Choice {
    fn window(w: &ask::Window, depth: u8) -> Self {
        Choice {
            row: window::Row {
                label: w.label(),
                depth,
                choosable: true,
            },
            source: capture::Source::Window(w.id),
            answer: Some(format!("Window: {}", w.id)),
        }
    }
}

fn pick() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let monitors = ask::shareable()?;
    if monitors.is_empty() {
        return Err("nothing to share".into());
    }
    // Each monitor, and under it its workspaces with their windows (or
    // with none, its windows).
    let mut choices = Vec::new();
    for monitor in &monitors {
        choices.push(Choice {
            row: window::Row {
                label: monitor.label(),
                depth: 0,
                choosable: true,
            },
            source: capture::Source::Monitor(monitor.name.clone()),
            answer: Some(format!("Monitor: {}", monitor.name)),
        });
        choices.extend(monitor.windows.iter().map(|w| Choice::window(w, 1)));
        for workspace in &monitor.workspaces {
            choices.push(Choice {
                row: window::Row {
                    label: workspace.name.clone(),
                    depth: 1,
                    choosable: false,
                },
                source: capture::Source::Workspace(workspace.id),
                answer: None,
            });
            choices.extend(workspace.windows.iter().map(|w| Choice::window(w, 2)));
        }
    }
    let sources: Vec<_> = choices.iter().map(|c| c.source.clone()).collect();
    let (captures, queue) = capture::Captures::start(&sources, window::THUMBNAIL_FRAMES)?;
    let rows = choices.iter().map(|c| c.row.clone()).collect();
    let chosen = window::choose(rows, captures, queue)?;
    Ok(chosen.and_then(|at| choices[at].answer.clone()))
}
