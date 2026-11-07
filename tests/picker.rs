//! The picker wayluck draws itself, to ask the user what to share: what it
//! offers, choosing with the keys or the pointer, and that while it's up no
//! client gets the keyboard or the pointer.

mod harness;

use harness::{Session, wait_for, wayland::Window, wm::Wm};
use std::process::Child;

/// Keys, as evdev codes.
const KEY_ESC: u32 = 1;
const KEY_A: u32 = 30;
const KEY_ENTER: u32 = 28;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;

/// One of the picker's tiles, as the test control socket says it.
struct Tile {
    /// "monitor NAME", "workspace N" or "window ID".
    what: String,
    /// Its middle (global).
    middle: (i32, i32),
    selected: bool,
}

/// The picker's tiles, in order, once it's up.
fn tiles(session: &mut Session) -> Vec<Tile> {
    let up = session.picker_up();
    up.split("; ")
        .map(|tile| {
            let selected = tile.ends_with(" *");
            let tile = tile.trim_end_matches(" *");
            let (what, rect) = tile.split_once('@').unwrap();
            let (x, rest) = rect.split_once(',').unwrap();
            let (y, size) = rest.split_once(',').unwrap();
            let (w, h) = size.split_once('x').unwrap();
            let [x, y, w, h] = [x, y, w, h].map(|n| n.parse::<i32>().unwrap());
            Tile {
                what: what.into(),
                middle: (x + w / 2, y + h / 2),
                selected,
            }
        })
        .collect()
}

/// What the portal is told a tile is shared as.
fn shared_as(tile: &Tile) -> String {
    let (kind, name) = tile.what.split_once(' ').unwrap();
    match kind {
        "monitor" => format!("Monitor: {name}\n"),
        "window" => format!("Window: {name}\n"),
        _ => panic!("not shared as itself: {}", tile.what),
    }
}

/// Whoever asked, once they've heard: what they printed, and whether they
/// exited successfully (a choice) or not (cancelled).
fn answer(mut pick: Child) -> (String, bool) {
    let status = wait_for(|| pick.try_wait().unwrap(), "wayluck pick to exit");
    let mut out = String::new();
    std::io::Read::read_to_string(&mut pick.stdout.take().unwrap(), &mut out).unwrap();
    (out, status.success())
}

#[test]
fn the_picker_offers_the_monitor_and_its_windows() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    let pick = session.pick();
    let offered = session.picker_up();
    let offered: Vec<&str> = offered.split("; ").collect();
    assert!(offered[0].starts_with("monitor "), "{offered:?}");
    assert_eq!(offered.len(), 2, "{offered:?}");
    assert!(offered[1].starts_with("window "), "{offered:?}");
    drop(pick);
}

#[test]
fn escape_closes_the_picker_choosing_nothing() {
    let mut session = Session::start();
    let pick = session.pick();
    session.picker_up();
    session.type_keys(&[KEY_ESC]);
    assert_eq!(answer(pick), (String::new(), false));
    assert_eq!(session.command("picker"), "-");
}

#[test]
fn while_the_picker_is_up_no_client_gets_input() {
    let mut session = Session::start();
    let window = Window::open(&session, 0xff00ff00);
    session.click_window("victim");
    wait_for(|| window.seen().focused.then_some(()), "the window's focus");
    let pick = session.pick();
    session.picker_up();
    session.wait_for(
        || (!window.seen().pointed).then_some(()),
        "the pointer to leave",
    );
    session.type_keys(&[KEY_A]);
    // On the window, but between the picker's tiles: nothing's chosen, and
    // the window gets nothing.
    session.click(5, 5);
    assert_ne!(
        session.command("picker"),
        "-",
        "a click between tiles chose"
    );
    assert!(
        !window.seen().keys.contains(&KEY_A),
        "a key went past the picker"
    );
    assert!(!window.seen().pointed, "the pointer went past the picker");

    session.type_keys(&[KEY_ESC]);
    answer(pick);
    session.type_keys(&[KEY_A]);
    session.wait_for(
        || window.seen().keys.contains(&KEY_A).then_some(()),
        "the key to reach the window again",
    );
    assert!(
        !window.seen().keys.contains(&KEY_ESC),
        "Escape went past the picker"
    );
}

#[test]
fn the_picker_closes_when_whoever_asked_goes() {
    let mut session = Session::start();
    let mut pick = session.pick();
    session.picker_up();
    pick.kill().unwrap();
    pick.wait().unwrap();
    wait_for(
        || (session.command("picker") == "-").then_some(()),
        "the picker to close",
    );
}

#[test]
fn enter_shares_the_selected_tile_the_first_at_first() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    let pick = session.pick();
    let tiles = tiles(&mut session);
    assert!(tiles[0].selected && tiles[0].what.starts_with("monitor "));
    session.type_keys(&[KEY_ENTER]);
    assert_eq!(answer(pick), (shared_as(&tiles[0]), true));
    assert_eq!(session.command("picker"), "-");
}

#[test]
fn the_arrows_move_the_selection() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    let pick = session.pick();
    // Left from the first: it stays.
    session.type_keys(&[KEY_LEFT]);
    assert!(tiles(&mut session)[0].selected);
    session.type_keys(&[KEY_RIGHT]);
    let tiles = tiles(&mut session);
    assert!(tiles[1].selected && !tiles[0].selected);
    session.type_keys(&[KEY_ENTER]);
    assert_eq!(answer(pick), (shared_as(&tiles[1]), true));
}

#[test]
fn pointing_at_a_tile_selects_it_and_clicking_shares_it() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    let pick = session.pick();
    let window = tiles(&mut session).remove(1);
    let (x, y) = window.middle;
    session.point(x, y);
    assert!(tiles(&mut session)[1].selected);
    session.click(x, y);
    assert_eq!(answer(pick), (shared_as(&window), true));
}

#[test]
fn a_workspace_is_shared_through_a_virtual_output() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _window = Window::open(&session, 0xff00ff00);
    wm.window("victim");
    let pick = session.pick();
    let tiles = tiles(&mut session);
    let what: Vec<&str> = tiles
        .iter()
        .map(|t| t.what.split(' ').next().unwrap())
        .collect();
    assert_eq!(what, ["monitor", "workspace", "window"]);
    let (x, y) = tiles[1].middle;
    session.click(x, y);
    let (said, chosen) = answer(pick);
    assert!(chosen, "the workspace wasn't shared");
    // Shared as an output of its own, not the monitor it's on.
    assert!(said.starts_with("Monitor: "), "{said}");
    assert_ne!(said, shared_as(&tiles[0]));
    wait_for(
        || (wm.shared() == 1).then_some(()),
        "the window manager to hear",
    );
}

#[test]
fn one_question_at_a_time() {
    let mut session = Session::start();
    let first = session.pick();
    session.picker_up();
    // Asked again meanwhile: cancelled at once; the first is still asked.
    assert_eq!(answer(session.pick()), (String::new(), false));
    assert_ne!(session.command("picker"), "-");
    session.type_keys(&[KEY_ENTER]);
    let (said, chosen) = answer(first);
    assert!(chosen && said.starts_with("Monitor: "), "{said}");
}
