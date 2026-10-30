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

/// One thing the user can choose: how it's shown, and what's printed.
struct Choice {
    row: window::Row,
    source: capture::Source,
    answer: String,
}

fn pick() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let monitors = ask::shareable()?;
    if monitors.is_empty() {
        return Err("nothing to share".into());
    }
    // Each monitor, and under it its windows.
    let mut choices = Vec::new();
    for monitor in &monitors {
        choices.push(Choice {
            row: window::Row {
                label: monitor.label(),
                indented: false,
            },
            source: capture::Source::Monitor(monitor.name.clone()),
            answer: format!("Monitor: {}", monitor.name),
        });
        for w in &monitor.windows {
            choices.push(Choice {
                row: window::Row {
                    label: w.label(),
                    indented: true,
                },
                source: capture::Source::Window(w.id),
                answer: format!("Window: {}", w.id),
            });
        }
    }
    let sources: Vec<_> = choices.iter().map(|c| c.source.clone()).collect();
    let (captures, queue) = capture::Captures::start(&sources, window::THUMBNAIL_FRAMES)?;
    let rows = choices.iter().map(|c| c.row.clone()).collect();
    let chosen = window::choose(rows, captures, queue)?;
    Ok(chosen.map(|at| choices[at].answer.clone()))
}
