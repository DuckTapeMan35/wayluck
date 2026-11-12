//! Wayland bars' view of the window manager's workspaces (ext-workspace-v1).

mod harness;
use harness::{Session, wait_for, wm::Wm, workspace_bar::Bar};

/// A workspace as a bar sees it, grouped: name, whether it's active, and
/// its number.
fn seen(name: &str, active: bool, number: u32) -> harness::workspace_bar::Workspace {
    harness::workspace_bar::Workspace {
        name: name.into(),
        active,
        number: Some(number),
        grouped: true,
    }
}

#[test]
fn a_bar_sees_the_window_managers_workspaces() {
    let session = Session::start();
    let wm = Wm::start(&session);
    wm.add_workspace("one");
    let bar = Bar::open(&session);
    wm.add_workspace("two");
    wm.activate_workspace("two");
    let both = [seen("one", false, 0), seen("two", true, 1)];
    bar.wait_for("one, and two active", |w| w == both);
    // On no output in particular: one group, on the one monitor.
    assert_eq!(bar.groups().len(), 1);
    assert_eq!(bar.groups()[0].outputs, 1);

    wm.activate_workspace("one");
    let switched = [seen("one", true, 0), seen("two", false, 1)];
    bar.wait_for("one active", |w| w == switched);
    // The one before it gone, it's the first.
    wm.remove_workspace("one");
    bar.wait_for("only two, first", |w| w == [seen("two", false, 0)]);
    // Gone, its workspaces go with it, and their group.
    wm.quit();
    bar.wait_for("none", |w| w.is_empty());
    wait_for(|| bar.groups().is_empty().then_some(()), "no group");
}

#[test]
fn a_bar_asking_to_show_a_workspace_asks_the_window_manager() {
    let session = Session::start();
    let wm = Wm::start(&session);
    wm.add_workspace("one");
    wm.add_workspace("two");
    let bar = Bar::open(&session);
    bar.wait_for("both", |w| w.len() == 2);
    // Its output bound after it was told: the group's on it still.
    wait_for(
        || (bar.groups().first().map(|g| g.outputs) == Some(1)).then_some(()),
        "the group on the monitor",
    );
    bar.activate("two");
    wait_for(
        || (wm.workspaces_asked() == ["two"]).then_some(()),
        "two asked to be shown",
    );
    // Only asked: the window manager decides.
    assert_eq!(bar.workspaces()[1], seen("two", false, 1));
}
