//! Layer surfaces (bars, wallpapers, notifications) as the window manager
//! hears of them.

mod harness;

use harness::{
    Session,
    capture::{Screenshot, screenshot},
    layer::{Kind, Layer},
    wait_for,
    wm::Wm,
};

/// The layers the window manager hears (wayluck_wm_layer_v1's layer).
const TOP: u32 = 2;

const RED: u32 = 0xff0000;
const GREEN: u32 = 0x00ff00;

/// The first screenshot in which f holds.
fn shown(session: &Session, what: &str, f: impl Fn(&Screenshot) -> bool) -> Screenshot {
    wait_for(
        || {
            let shot = screenshot(session);
            f(&shot).then_some(shot)
        },
        what,
    )
}

#[test]
fn the_window_manager_hears_of_a_bar() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _bar = Layer::open(&session, "testbar", Kind::Top, 30, 0xff202020);
    let heard = wm.layer("testbar");
    assert_eq!(heard.layer, Some(TOP));
    assert!(heard.on_output);
}

#[test]
fn a_bar_there_before_the_window_manager_is_heard_of_too() {
    let session = Session::start();
    let _bar = Layer::open(&session, "early", Kind::Top, 30, 0xff202020);
    let wm = Wm::start(&session);
    assert_eq!(wm.layer("early").layer, Some(TOP));
}

#[test]
fn a_bar_gone_is_closed() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let bar = Layer::open(&session, "brief", Kind::Top, 30, 0xff202020);
    wm.layer("brief");
    drop(bar);
    wm.wait_for_layer("brief", "it closed", |l| l.closed);
}

#[test]
fn a_bars_corners_are_cut_off() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _bar = Layer::open(&session, "round", Kind::Top, 40, 0xff00ff00);
    wm.layer("round");
    let shot = shown(&session, "the bar", |s| s.at(20, 20) == GREEN);
    assert_eq!(shot.at(1, 1), GREEN);
    wm.layer_corners("round", 20);
    let shot = shown(&session, "its corner cut off", |s| s.at(1, 1) != GREEN);
    assert_eq!(
        shot.at(shot.width - 2, 1),
        shot.at(1, 1),
        "the other corner"
    );
    assert_eq!(shot.at(20, 20), GREEN, "inside");
    assert_eq!(shot.at(shot.width / 2, 1), GREEN, "its edge");
}

#[test]
fn a_bar_has_a_shadow_below_it() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _bar = Layer::open(&session, "shady", Kind::Top, 40, 0xff00ff00);
    wm.layer("shady");
    let shot = shown(&session, "the bar", |s| s.at(20, 20) == GREEN);
    let below = shot.at(20, 50);
    assert_ne!(below, RED);
    wm.layer_shadow("shady", 0, 0, (0, 20), 0xffff0000);
    let shot = shown(&session, "the shadow", |s| s.at(20, 50) == RED);
    assert_eq!(shot.at(20, 20), GREEN, "the bar over it");
    assert_eq!(shot.at(20, 70), below, "past it");
}
