//! The on-screen path: drawing into a CAMetalLayer that is not in any window.

use objc2_core_foundation::CGSize;
use objc2_metal::MTLPixelFormat;
use objc2_quartz_core::CAMetalLayer;
use tern_render::{FontConfig, PaneDraw, Present, Quad, Renderer, Scene};
use tern_types::{Frame, Point, Rect};

#[test]
fn draws_into_a_layer_with_both_present_modes() {
    let mut r = Renderer::new(&FontConfig {
        family: "Menlo".into(),
        size_pt: 13.0,
    })
    .unwrap();
    let layer = CAMetalLayer::new();
    r.configure_layer(&layer);
    assert_eq!(layer.pixelFormat(), MTLPixelFormat::BGRA8Unorm);
    assert!(layer.framebufferOnly());
    layer.setDrawableSize(CGSize {
        width: 320.0,
        height: 200.0,
    });

    let mut frame = Frame {
        fg: 0xffffff,
        bg: 0x000000,
        ..Frame::default()
    };
    frame.resize(20, 5);
    frame.row_mut(0)[0].ch = 'x' as u32;
    let panes = [PaneDraw {
        id: 1,
        rect: Rect::new(0, 0, 320, 200),
        origin: Point { x: 2, y: 2 },
        frame: &frame,
        focused: true,
        cursor_on: true,
        dim: 0.0,
    }];
    let quads = [Quad {
        rect: Rect::new(10, 10, 5, 5),
        color: 0xff0000,
        alpha: 1.0,
    }];
    let scene = Scene {
        width: 320,
        height: 200,
        scale: 1.0,
        clear: 0,
        panes: &panes,
        quads: &quads,
    };
    for _ in 0..5 {
        assert!(r.draw(&layer, &scene, Present::Async));
    }
    layer.setPresentsWithTransaction(true);
    assert!(r.draw(&layer, &scene, Present::WithTransaction));
    // A scene larger than the drawable is clipped, not rejected.
    let big = Scene {
        width: 640,
        height: 400,
        ..scene
    };
    assert!(r.draw(&layer, &big, Present::Async));
    // Nothing to draw.
    let empty = Scene {
        width: 0,
        height: 0,
        ..scene
    };
    assert!(r.draw(&layer, &empty, Present::Async));
}

#[test]
fn prepares_on_another_thread_and_notifies_presentation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::MTLDrawable;

    let font = FontConfig {
        family: "Menlo".into(),
        size_pt: 13.0,
    };
    let prepared = std::thread::spawn(move || Renderer::prepare(&font))
        .join()
        .unwrap()
        .unwrap();
    let mut r = Renderer::from_prepared(prepared);
    assert_eq!(r.font().family, "Menlo");
    let layer = CAMetalLayer::new();
    r.configure_layer(&layer);
    layer.setDrawableSize(CGSize {
        width: 64.0,
        height: 32.0,
    });
    let mut frame = Frame::default();
    frame.resize(4, 1);
    let panes = [PaneDraw {
        id: 1,
        rect: Rect::new(0, 0, 64, 32),
        origin: Point { x: 0, y: 0 },
        frame: &frame,
        focused: true,
        cursor_on: true,
        dim: 0.0,
    }];
    let scene = Scene {
        width: 64,
        height: 32,
        scale: 1.0,
        clear: 0,
        panes: &panes,
        quads: &[],
    };
    // One handler serves every frame; Metal calls it whether the drawable
    // was shown or dropped (a layer in no window drops them).
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let handler = RcBlock::new(
        move |_: std::ptr::NonNull<ProtocolObject<dyn MTLDrawable>>| {
            counter.fetch_add(1, Ordering::Relaxed);
        },
    );
    for _ in 0..3 {
        assert!(r.draw_notify(&layer, &scene, Present::Async, Some(&handler)));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while calls.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}
