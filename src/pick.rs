//! Choosing what to share. `wayluck pick` is xdg-desktop-portal-wlr's
//! chooser_cmd: it asks wayluck and prints the answer the way
//! the portal wants it ("Monitor: NAME", "Window: ID"). wayluck asks the
//! user by starting `wayluck picker` on a connection it makes, the only one
//! allowed to see what can be shared: it shows a window
//! with live thumbnails , which the window manager places like
//! any other (fixed size: floating), and gives wayluck the user's choice.

mod ask;
mod capture;
mod chooser;
mod text;
mod window;

/// Run `wayluck pick`. The exit code: 0 with the choice printed, or 1 if
/// the user cancelled or on an error (said on stderr): the portal shares
/// nothing then.
pub fn run_pick() -> i32 {
    match chooser::ask() {
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

/// Run `wayluck picker`, as wayluck starts it. The exit code: 0, or 1 on
/// an error (said on stderr; wayluck takes it as cancelled).
pub fn run_picker() -> i32 {
    match picker() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("wayluck picker: {e}");
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

fn picker() -> Result<(), Box<dyn std::error::Error>> {
    // Until answered (or dropped, on an error: cancelled).
    let (monitors, host) = ask::shareable()?;
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
    let fit = window::THUMBNAIL_FRAMES;
    let (captures, queue) =
        capture::Captures::start(host.connection(), host.proxy(), &sources, fit)?;
    let rows = choices.iter().map(|c| c.row.clone()).collect();
    let (chosen, host) = window::choose(rows, host, captures, queue)?;
    host.answer(chosen.and_then(|at| choices[at].answer.clone()));
    Ok(())
}
