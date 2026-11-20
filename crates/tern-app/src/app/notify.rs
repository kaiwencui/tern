//! Background notifications (OSC 9 / OSC 777) via UNUserNotificationCenter and
//! Dock attention bounce.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::msg_send;
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
use objc2_foundation::NSString;
use tern_types::PaneId;

use super::App;
use crate::identity;

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

static AUTH_REQUESTED: AtomicBool = AtomicBool::new(false);
static NEXT_NOTIFY_ID: AtomicU64 = AtomicU64::new(1);

impl App {
    pub fn pane_notify(&self, pane: PaneId, title: Option<String>, body: String) {
        let heading = title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| identity::app_name().to_owned());

        if self.records_input() {
            self.notifications
                .borrow_mut()
                .push(format!("{heading}: {body}"));
            return;
        }

        let ns = NSApplication::sharedApplication(self.mtm);
        if ns.isActive() {
            let active_in_front = self
                .focused
                .get()
                .and_then(|id| self.state(id))
                .is_some_and(|s| s.focused_pane_id() == Some(pane));
            if active_in_front {
                return;
            }
        }

        ns.requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        post_macos_notification(&heading, &body);
    }
}

fn post_macos_notification(title: &str, body: &str) {
    // UNUserNotificationCenter throws an Objective-C exception if invoked from
    // an unbundled executable without a CFBundleIdentifier.
    if !identity::is_bundled() {
        return;
    }
    let (Some(center_cls), Some(content_cls), Some(req_cls)) = (
        AnyClass::get(c"UNUserNotificationCenter"),
        AnyClass::get(c"UNMutableNotificationContent"),
        AnyClass::get(c"UNNotificationRequest"),
    ) else {
        return;
    };

    // SAFETY: messaging standard UserNotifications classes inside a valid app bundle.
    unsafe {
        let center: Retained<NSObject> = msg_send![center_cls, currentNotificationCenter];
        if !AUTH_REQUESTED.swap(true, Ordering::Relaxed) {
            let handler = RcBlock::new(|_granted: Bool, _err: *mut NSObject| {});
            // UNAuthorizationOptionBadge (1) | Sound (2) | Alert (4) = 7.
            let _: () = msg_send![
                &center,
                requestAuthorizationWithOptions: 7usize,
                completionHandler: &*handler
            ];
        }

        let content: Retained<NSObject> = msg_send![msg_send![content_cls, alloc], init];
        let ns_title = NSString::from_str(title);
        let ns_body = NSString::from_str(body);
        let _: () = msg_send![&content, setTitle: &*ns_title];
        let _: () = msg_send![&content, setBody: &*ns_body];

        let id = format!("tern-{}", NEXT_NOTIFY_ID.fetch_add(1, Ordering::Relaxed));
        let ns_id = NSString::from_str(&id);
        let req: Retained<NSObject> = msg_send![
            req_cls,
            requestWithIdentifier: &*ns_id,
            content: &*content,
            trigger: Option::<&NSObject>::None
        ];
        let _: () = msg_send![
            &center,
            addNotificationRequest: &*req,
            withCompletionHandler: Option::<&AnyObject>::None
        ];
    }
}
