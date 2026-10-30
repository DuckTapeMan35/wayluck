//! `wayluck pick`: xdg-desktop-portal-wlr's chooser_cmd. When an app asks to
//! share the screen, the portal runs it: it asks wayluck what can be shared,
//! shows the user a picker with live thumbnails, and prints the choice the
//! way the portal wants it ("Monitor: NAME"). The picker is a window like
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

fn pick() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let monitors = ask::shareable()?;
    if monitors.is_empty() {
        return Err("nothing to share".into());
    }
    let labels = monitors.iter().map(ask::Monitor::label).collect();
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let (captures, queue) = capture::Captures::start(&names)?;
    let chosen = window::choose(labels, captures, queue)?;
    Ok(chosen.map(|at| format!("Monitor: {}", monitors[at].name)))
}
