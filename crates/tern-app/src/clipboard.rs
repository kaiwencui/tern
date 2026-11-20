//! Every use of a pasteboard: the clipboard Copy and Paste use (the
//! general pasteboard, or a private named one given with --pasteboard), and
//! reading what a drag and drop delivered.

use std::path::PathBuf;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::ClassType;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardType, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardTypeURL, NSPasteboardURLReadingFileURLsOnlyKey,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use crate::input::DropPayload;

pub struct Clipboard {
    pasteboard: Retained<NSPasteboard>,
    /// A private pasteboard, which outlives the process unless released.
    named: bool,
}

impl Clipboard {
    /// The general pasteboard, or the named one.
    pub fn new(name: Option<&str>) -> Clipboard {
        match name {
            Some(name) => Clipboard {
                pasteboard: NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
                named: true,
            },
            None => Clipboard {
                pasteboard: NSPasteboard::generalPasteboard(),
                named: false,
            },
        }
    }

    pub fn text(&self) -> Option<String> {
        // SAFETY: the pasteboard type static is initialized by AppKit.
        let text = self
            .pasteboard
            .stringForType(unsafe { NSPasteboardTypeString });
        text.map(|t| t.to_string())
    }

    /// Whether there is text to paste, without reading it.
    pub fn has_text(&self) -> bool {
        // SAFETY: as above.
        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
        self.pasteboard.availableTypeFromArray(&types).is_some()
    }

    pub fn set_text(&self, text: &str) {
        self.pasteboard.clearContents();
        // SAFETY: as above.
        let ok = self
            .pasteboard
            .setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        if !ok {
            eprintln!("tern: could not write to the clipboard");
        }
    }

    /// Lets go of a private pasteboard; the app is quitting.
    pub fn release(&self) {
        if self.named {
            // SAFETY: releaseGlobally takes no arguments; the pasteboard is
            // not used after, as the app is terminating.
            let () = unsafe { msg_send![&*self.pasteboard, releaseGlobally] };
        }
    }
}

/// The kinds of data a drag may bring for the view to take.
pub fn drop_types() -> Retained<NSArray<NSPasteboardType>> {
    // SAFETY: the pasteboard type statics are initialized by AppKit.
    unsafe {
        NSArray::from_slice(&[
            NSPasteboardTypeFileURL,
            NSPasteboardTypeURL,
            NSPasteboardTypeString,
        ])
    }
}

/// What a drag delivered: file URLs become paths; anything else is taken as
/// text.
pub fn read_drop(pasteboard: &NSPasteboard) -> Option<DropPayload> {
    let url_class: &AnyClass = NSURL::class();
    let classes = NSArray::from_slice(&[url_class]);
    let yes = NSNumber::new_bool(true);
    // SAFETY: the option key static is initialized by AppKit.
    let key = unsafe { NSPasteboardURLReadingFileURLsOnlyKey };
    let file_urls_only = NSDictionary::from_slices(&[key], &[yes.as_ref() as &AnyObject]);
    // SAFETY: NSURL implements NSPasteboardReading; the option is one
    // NSPasteboard documents for it.
    let files =
        unsafe { pasteboard.readObjectsForClasses_options(&classes, Some(&file_urls_only)) };
    let paths: Vec<PathBuf> = files
        .iter()
        .flat_map(|array| array.to_vec())
        .filter_map(|object| object.downcast::<NSURL>().ok())
        .filter_map(|url| url.path())
        .map(|path| PathBuf::from(path.to_string()))
        .collect();
    if !paths.is_empty() {
        return Some(DropPayload::Paths(paths));
    }
    // SAFETY: the pasteboard type static is initialized by AppKit.
    if let Some(text) = pasteboard.stringForType(unsafe { NSPasteboardTypeString }) {
        return Some(DropPayload::Text(text.to_string()));
    }
    // Some sources offer a link only as a URL.
    // SAFETY: as above, without options.
    let urls = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
    urls.iter()
        .flat_map(|array| array.to_vec())
        .filter_map(|object| object.downcast::<NSURL>().ok())
        .find_map(|url| url.absoluteString())
        .map(|s| DropPayload::Text(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::NSPasteboardWriting;

    /// A private pasteboard holding these objects, so the test never touches
    /// the user's clipboard.
    fn pasteboard(objects: &[&ProtocolObject<dyn NSPasteboardWriting>]) -> Retained<NSPasteboard> {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        pasteboard.clearContents();
        if !objects.is_empty() {
            assert!(pasteboard.writeObjects(&NSArray::from_slice(objects)));
        }
        pasteboard
    }

    fn release(pasteboard: Retained<NSPasteboard>) {
        // SAFETY: releaseGlobally takes no arguments; the pasteboard is not used after.
        let () = unsafe { msg_send![&pasteboard, releaseGlobally] };
    }

    fn file_url(path: &str) -> Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(path))
    }

    #[test]
    fn a_named_clipboard_keeps_text_to_itself() {
        let name = format!("tern-unit-{}", std::process::id());
        let clipboard = Clipboard::new(Some(&name));
        clipboard.pasteboard.clearContents();
        assert!(!clipboard.has_text());
        assert_eq!(clipboard.text(), None);
        clipboard.set_text("copied ✓\n");
        assert!(clipboard.has_text());
        assert_eq!(clipboard.text().as_deref(), Some("copied ✓\n"));
        // The same name reaches the same pasteboard.
        assert_eq!(
            Clipboard::new(Some(&name)).text().as_deref(),
            Some("copied ✓\n")
        );
        clipboard.release();
    }

    #[test]
    fn dropped_files_become_paths() {
        let a = file_url("/tmp/a file.txt");
        let b = file_url("/Users/x/Документы");
        let pb = pasteboard(&[ProtocolObject::from_ref(&*a), ProtocolObject::from_ref(&*b)]);
        assert_eq!(
            read_drop(&pb),
            Some(DropPayload::Paths(vec![
                PathBuf::from("/tmp/a file.txt"),
                PathBuf::from("/Users/x/Документы"),
            ]))
        );
        release(pb);
    }

    #[test]
    fn dropped_text_and_links_become_text() {
        let text = NSString::from_str("echo hi\n");
        let pb = pasteboard(&[ProtocolObject::from_ref(&*text)]);
        assert_eq!(read_drop(&pb), Some(DropPayload::Text("echo hi\n".into())));
        release(pb);

        let link = NSURL::URLWithString(&NSString::from_str("https://example.com/a?b=c"))
            .expect("a valid URL");
        let pb = pasteboard(&[ProtocolObject::from_ref(&*link)]);
        assert_eq!(
            read_drop(&pb),
            Some(DropPayload::Text("https://example.com/a?b=c".into()))
        );
        release(pb);

        let pb = pasteboard(&[]);
        assert_eq!(read_drop(&pb), None);
        release(pb);
    }
}
