//! Screen capture, as screen sharing does it.

mod harness;

use harness::{Session, capture::capture_output};
use std::time::Duration;

/// How long a frame of something still may take to come.
const STILL: Duration = Duration::from_secs(1);

#[test]
fn a_still_screen_keeps_coming() {
    let session = Session::start();
    let frames = capture_output(&session, 4);
    // The first at once; the rest though nothing on screen changes (an app
    // dropping frames while it starts would otherwise wait for a change).
    for n in 0..4 {
        let took = frames
            .recv_timeout(STILL)
            .unwrap_or_else(|_| panic!("frame {n} didn't come within {STILL:?}"));
        assert!(took <= STILL, "frame {n} took {took:?}");
    }
}
