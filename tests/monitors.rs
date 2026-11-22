//! Monitors coming (and going): where they go, and who hears.

mod harness;

use harness::{
    Session,
    layer::{Kind, Layer},
    output_manager::{OutputManager, Wanted},
    wait_for,
    wayland::Window,
    wm::Wm,
};
use wayland_client::protocol::wl_output::Transform;

#[test]
fn a_monitor_plugged_in_goes_right_of_the_others() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let first = wm.output("HEADLESS-1").geometry.unwrap();
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    // The window manager hears of it, where it is: its top lined up with
    // the first's, its left at the first's right.
    let second = wm.output("HEADLESS-2").geometry.unwrap();
    assert_eq!(second, (first.0 + first.2, first.1, 800, 600));
}

#[test]
fn the_pointer_goes_across_monitors_and_no_further() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    session.command("move 1270 100");
    // Onto the second, right of the first (1280 wide).
    assert_eq!(session.command("nudge 20 0"), "1290,100");
    // Below the second (600 high), there's nothing: it stays on it.
    assert_eq!(session.command("nudge 0 700"), "1290,599");
}

#[test]
fn virtual_outputs_stay_right_of_the_monitors() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    wm.virtual_output(640, 480);
    assert_eq!(wm.output("VIRTUAL-1").geometry, Some((1280, 0, 640, 480)));
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    // Moved over to make room: the pointer, kept on the monitors, never
    // gets there.
    wait_for(
        || (wm.output("VIRTUAL-1").geometry == Some((2080, 0, 640, 480))).then_some(()),
        "the virtual output right of the new monitor",
    );
}

#[test]
fn an_unplugged_monitor_is_gone_and_the_ones_right_of_it_close_up() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    wm.output("HEADLESS-2");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    wait_for(
        || wm.output("HEADLESS-1").removed.then_some(()),
        "the window manager to hear it's gone",
    );
    wait_for(
        || (wm.output("HEADLESS-2").geometry == Some((0, 0, 800, 600))).then_some(()),
        "the second where the first was",
    );
}

#[test]
fn an_unplugged_monitors_bars_close() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    // On the first monitor, as a bar that names none is.
    let bar = Layer::open(&session, "bar", Kind::Top, 30, 0xff3366cc);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    wait_for(|| bar.closed().then_some(()), "the bar to be closed");
}

#[test]
fn the_pointer_on_an_unplugged_monitor_comes_back_onto_one() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    session.command("move 1300 100");
    assert_eq!(session.command("remove-monitor HEADLESS-2"), "ok");
    // At the edge of the first, as near as it gets: from there, 10 left is
    // still on it.
    assert_eq!(session.command("nudge -10 0"), "1269,100");
}

#[test]
fn with_no_window_manager_windows_go_to_the_first_monitor_left() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    session.window("victim");
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    // Still on screen: on the one that's left, now where the first was.
    let (x, y, _, _) = session.window("victim");
    assert!(
        x < 800 && y < 600,
        "it isn't on the monitor left: at {x},{y}"
    );
}

#[test]
fn a_tool_hears_of_every_monitor_its_modes_and_where_it_is() {
    let mut session = Session::start();
    let tool = OutputManager::open(&session);
    let first = tool.head("HEADLESS-1");
    assert!(first.enabled);
    assert_eq!(first.position, (0, 0));
    assert_eq!(first.scale, 1.0);
    let sizes: Vec<_> = first.modes.iter().map(|m| m.size).collect();
    assert!(sizes.contains(&(1280, 800)) && sizes.contains(&(1920, 1080)));
    let current = first.mode.unwrap();
    assert_eq!(current.size, (1280, 800));
    assert!(current.preferred);
    // One plugged in is told of, and so is its going.
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    let heads = tool.wait_for("the second head", |h| h.len() == 2);
    assert_eq!(heads[1].position, (1280, 0));
    assert_eq!(session.command("remove-monitor HEADLESS-2"), "ok");
    tool.wait_for("the second head to go", |h| h.len() == 1);
}

#[test]
fn a_tool_puts_monitors_where_it_wants_them() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    let tool = OutputManager::open(&session);
    // The second left of the first, lower down.
    let left = Wanted {
        position: Some((0, 200)),
        ..Default::default()
    };
    let right = Wanted {
        position: Some((800, 0)),
        ..Default::default()
    };
    assert_eq!(
        tool.apply(&[("HEADLESS-2", left), ("HEADLESS-1", right)]),
        "succeeded"
    );
    // The tool, the window manager and the pointer go by it.
    tool.wait_for("the heads moved", |h| {
        h.iter()
            .any(|h| h.name == "HEADLESS-2" && h.position == (0, 200))
    });
    wait_for(
        || (wm.output("HEADLESS-2").geometry == Some((0, 200, 800, 600))).then_some(()),
        "the window manager to hear",
    );
    assert_eq!(wm.output("HEADLESS-1").geometry, Some((800, 0, 1280, 800)));
    session.command("move 810 100");
    assert_eq!(session.command("nudge -20 150"), "790,250");
    // One plugged in after goes right of them.
    assert_eq!(session.command("add-monitor HEADLESS-3 640 480"), "ok");
    let third = wm.output("HEADLESS-3").geometry.unwrap();
    assert_eq!(third, (2080, 0, 640, 480));
}

#[test]
fn a_tool_shows_a_monitor_at_another_of_its_modes() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let tool = OutputManager::open(&session);
    let bigger = Wanted {
        mode: Some((1920, 1080)),
        ..Default::default()
    };
    // Tested, nothing changes; applied, it does.
    assert_eq!(tool.test(&[("HEADLESS-1", bigger)]), "succeeded");
    assert_eq!(tool.head("HEADLESS-1").mode.unwrap().size, (1280, 800));
    assert_eq!(tool.apply(&[("HEADLESS-1", bigger)]), "succeeded");
    tool.wait_for("the new mode", |h| {
        h[0].mode.is_some_and(|m| m.size == (1920, 1080))
    });
    wait_for(
        || (wm.output("HEADLESS-1").geometry == Some((0, 0, 1920, 1080))).then_some(()),
        "the window manager to hear",
    );
    // It's drawn at that size: a capture of it is.
    let shot = harness::capture::screenshot(&session);
    assert_eq!((shot.width, shot.height), (1920, 1080));
    // A custom mode is one of its own, or not at all.
    let own = Wanted {
        custom_mode: Some((1024, 768, 0)),
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-1", own)]), "succeeded");
    let other = Wanted {
        custom_mode: Some((1000, 700, 0)),
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-1", other)]), "failed");
}

#[test]
fn a_tool_turns_and_scales_a_monitor() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let tool = OutputManager::open(&session);
    let turned = Wanted {
        transform: Some(Transform::_90),
        scale: Some(2.0),
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-1", turned)]), "succeeded");
    let head = tool.wait_for("it turned", |h| h[0].scale == 2.0).remove(0);
    assert_eq!(head.transform, Some(Transform::_90));
    // 1280x800 turned, halved.
    wait_for(
        || (wm.output("HEADLESS-1").geometry == Some((0, 0, 400, 640))).then_some(()),
        "the window manager to hear",
    );
}

#[test]
fn a_configuration_made_before_monitors_changed_is_cancelled() {
    let mut session = Session::start();
    let tool = OutputManager::open(&session);
    let serial = tool.serial();
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    tool.wait_for("the second head", |h| h.len() == 2);
    let moved = Wanted {
        position: Some((5000, 0)),
        ..Default::default()
    };
    assert_eq!(
        tool.apply_for(serial, &[("HEADLESS-1", moved)]),
        "cancelled"
    );
    assert_eq!(tool.head("HEADLESS-1").position, (0, 0));
}

#[test]
fn a_tool_disables_a_monitor_and_enables_it_again() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    wm.output("HEADLESS-2");
    let tool = OutputManager::open(&session);
    let off = Wanted {
        disabled: true,
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-1", off)]), "succeeded");
    // As if it went, for the window manager. The second stays where it
    // was: a head enabled without a place keeps the one it has.
    wait_for(
        || wm.output("HEADLESS-1").removed.then_some(()),
        "the window manager to hear it's gone",
    );
    let second = wm.output("HEADLESS-2").geometry.unwrap();
    assert_eq!(second, (1280, 0, 800, 600));
    // The pointer can't go onto it: it was moved off, onto the second.
    assert_eq!(session.command("nudge 0 0"), "1280,0");
    assert_eq!(session.command("nudge -100 10"), "1280,10");
    // The tool still has it, disabled, and enables it again.
    let heads = tool.wait_for("it disabled", |h| h.iter().any(|h| !h.enabled));
    assert_eq!(heads.len(), 2);
    let on = Wanted {
        position: Some((800, 0)),
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-1", on)]), "succeeded");
    let heads = tool.wait_for("it enabled", |h| h.iter().all(|h| h.enabled));
    let first = heads.iter().find(|h| h.name == "HEADLESS-1").unwrap();
    assert_eq!(first.position, (800, 0));
    assert_eq!(first.mode.unwrap().size, (1280, 800));
    wait_for(
        || {
            let back = wm.output("HEADLESS-1");
            (!back.removed && back.geometry == Some((800, 0, 1280, 800))).then_some(())
        },
        "the window manager to hear it's back",
    );
}

#[test]
fn a_disabled_monitor_unplugged_is_gone_for_the_tool_too() {
    let mut session = Session::start();
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    let tool = OutputManager::open(&session);
    let off = Wanted {
        disabled: true,
        ..Default::default()
    };
    assert_eq!(tool.apply(&[("HEADLESS-2", off)]), "succeeded");
    tool.wait_for("it disabled", |h| h.iter().any(|h| !h.enabled));
    assert_eq!(session.command("remove-monitor HEADLESS-2"), "ok");
    tool.wait_for("it gone", |h| h.len() == 1);
}

#[test]
fn disabling_every_monitor_fails_and_changes_nothing() {
    let mut session = Session::start();
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    let tool = OutputManager::open(&session);
    let off = Wanted {
        disabled: true,
        ..Default::default()
    };
    assert_eq!(
        tool.apply(&[("HEADLESS-1", off), ("HEADLESS-2", off)]),
        "failed"
    );
    assert!(tool.heads().iter().all(|h| h.enabled));
}
