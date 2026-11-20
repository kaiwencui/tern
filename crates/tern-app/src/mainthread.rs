//! Carrying work from other threads (sessions, the automation socket) over
//! to the main thread.

use std::cell::Cell;

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSOperationQueue, NSPoint};

/// Runs the job on the main thread soon. Callable from any thread.
///
/// The main queue runs jobs while NSApplication waits for an event, outside
/// any autorelease pool AppKit drains, so each job gets a pool of its own.
/// Objects AppKit itself autoreleases into its outer pool (a closed window,
/// its view and its layer) stay until the next event arrives; a user always
/// sends more, a program driving the app may not, so when wake is set an
/// empty event follows the job.
pub fn post_to_main(wake: bool, job: impl FnOnce(MainThreadMarker) + Send + 'static) {
    let job = Cell::new(Some(job));
    let block = RcBlock::new(move || {
        if let (Some(job), Some(mtm)) = (job.take(), MainThreadMarker::new()) {
            autoreleasepool(|_| job(mtm));
            if wake {
                wake_event_loop(mtm);
            }
        }
    });
    // SAFETY: the main queue runs the block once on the main thread, and the
    // block owns everything it uses.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

/// Posts an empty event, so NSApplication dequeues an event and drains the
/// autorelease pool it keeps around each one.
pub fn wake_event_loop(mtm: MainThreadMarker) {
    let wake = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(wake) = wake {
        NSApplication::sharedApplication(mtm).postEvent_atStart(&wake, false);
    }
}
