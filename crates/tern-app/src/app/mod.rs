//! The application: its delegate, the window list, the configuration and
//! the renderer every window shares, and perform(), the one place every
//! menu item, key binding and automation action ends up.

use std::cell::{Cell, OnceCell, RefCell, RefMut};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBeep, NSMenuItem, NSMenuItemValidation, NSPasteboard, NSWindow,
    NSWindowOrderingMode, NSWindowTabbingMode,
};
use objc2_foundation::{
    ns_string, NSActivityOptions, NSArray, NSDictionary, NSNotification, NSObject,
    NSObjectProtocol, NSPoint, NSProcessInfo, NSRect, NSString, NSUserDefaults, NSURL,
};
use tern_config::{Config, WorkingDirectory};
use tern_render::{FontConfig, Prepared, RenderError, Renderer};
use tern_types::PaneId;

mod actions;
mod close;
pub use actions::keep_in_front;
mod config;
mod context;
mod links;
mod moving;
mod notify;
mod placement;
mod rename;
mod session;
mod tabdrag;

pub use close::CloseTarget;
use close::Question;
pub use context::PaneContext;

use crate::automation;
use crate::clipboard::Clipboard;
use crate::mainthread::post_to_main;
use crate::menu::{self, AppCommand};
use crate::pane::{new_pane_id, FrameRequest, Launch, Pane, PaneEvents};
use crate::startup::StartupProbe;
use crate::state::WindowState;
use crate::view::TernView;
use crate::window::TernWindow;
use crate::Options;

/// The bell sounds at most this often.
const BELL_INTERVAL: Duration = Duration::from_secs(1);
const MIN_FONT_SIZE: f32 = 6.0;
const MAX_FONT_SIZE: f32 = 72.0;

/// Where a new window goes.
pub enum Placement {
    /// A window of its own.
    Window,
    /// A new tab in this window's tab group.
    TabOf(Retained<NSWindow>),
}

thread_local! {
    static APP: OnceCell<Rc<App>> = const { OnceCell::new() };
}

/// The application state. Main thread only.
pub fn app() -> Rc<App> {
    APP.with(|cell| cell.get().cloned())
        .expect("the app is set up before the run loop starts")
}

type RendererSetup = JoinHandle<(Result<Prepared, RenderError>, Duration)>;

pub struct App {
    pub mtm: MainThreadMarker,
    pub options: Options,
    /// Replaced as a whole when the configuration is reloaded.
    config: RefCell<Rc<Config>>,
    /// Built from the setup thread's work the first time it is needed.
    renderer: OnceCell<RefCell<Renderer>>,
    renderer_setup: RefCell<Option<RendererSetup>>,
    font_size: Cell<f32>,
    delegate: Retained<AppDelegate>,
    windows: RefCell<Vec<TernWindow>>,
    next_id: Cell<u64>,
    /// The window that most recently became key.
    focused: Cell<Option<u64>>,
    /// The command of each menu item, by the item's tag.
    menu_commands: RefCell<Vec<AppCommand>>,
    /// The actions of the currently open right-click context menu, by tag.
    context_actions: RefCell<Vec<context::ContextAction>>,
    startup: Option<Arc<StartupProbe>>,
    /// The first window's pane, started before AppKit is up.
    first_pane: RefCell<Option<JoinHandle<std::io::Result<Pane>>>>,
    restored_session: RefCell<Option<session::SavedSession>>,
    last_session: RefCell<Option<session::SavedSession>>,
    clipboard: Clipboard,
    /// Links Command-click opened while the automation socket is on, which
    /// records them instead of opening them.
    opened_links: RefCell<Vec<String>>,
    notifications: RefCell<Vec<String>>,
    last_bell: Cell<Option<Instant>>,
    bells: Cell<u64>,
    control: Option<automation::Server>,
    /// The user said yes to quitting with programs running.
    quit_confirmed: Cell<bool>,
    /// Questions waiting for the sheet already on their window to go.
    questions: RefCell<Vec<Question>>,
    /// Notices shown so far, which numbers each one.
    notices: Cell<u64>,
    /// The tab whose name is being typed, after a double click on it.
    renaming: RefCell<Option<rename::Rename>>,
    /// The tab the mouse holds in its tab bar, while AppKit drags it.
    tab_drag: RefCell<Option<tabdrag::TabDrag>>,
    /// Where the automation socket has the pointer while it drags a tab,
    /// and the number of the window its events go to.
    tab_pointer: Cell<Option<(isize, NSPoint)>>,
    /// Problems the configuration had at launch, shown once the first
    /// window is up.
    startup_problems: RefCell<Vec<String>>,
    /// Keeps App Nap off for the life of the app (see run).
    _activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

pub fn run(
    mtm: MainThreadMarker,
    options: Options,
    process_start: SystemTime,
) -> Result<(), String> {
    let startup = options
        .log_startup
        .then(|| Arc::new(StartupProbe::new(process_start)));
    let mark = |what: &str| {
        if let Some(probe) = &startup {
            probe.mark(what);
        }
    };
    let (config, diagnostics) = Config::load(options.config_file.as_deref(), &options.overrides);
    mark("configuration loaded");

    // The Metal device and pipeline take the longest of anything at
    // startup; they are made while AppKit starts up.
    let font = FontConfig {
        family: config.font_family.clone(),
        size_pt: config.font_size,
    };
    let renderer_setup = thread::Builder::new()
        .name("tern-renderer-setup".into())
        .spawn(move || {
            let start = Instant::now();
            (Renderer::prepare(&font), start.elapsed())
        })
        .map_err(|e| format!("cannot start a thread: {e}"))?;

    // Before the first session: binding the socket briefly changes the
    // umask, which a child started then would inherit.
    let control = match &options.control {
        Some(path) => Some(
            automation::Server::start(path)
                .map_err(|e| format!("control socket {}: {e}", path.display()))?,
        ),
        None => None,
    };

    let restored = session::load_saved(&config, &options);

    // The shell starts now, on a thread of its own, and reads its startup
    // files while AppKit and Metal get ready. The grid is the configured
    // window size in cells; the cell size in pixels follows once the window
    // knows its screen.
    let first_pane = {
        let config = config.clone();
        let initial_dir = options
            .paths
            .first()
            .and_then(|p| dir_for_path(p))
            .or_else(|| restored.as_ref().and_then(session::SavedSession::first_working_dir))
            .or_else(|| configured_dir(&config));
        let launch = Launch {
            id: new_pane_id(),
            command: (!options.command.is_empty()).then(|| options.command.clone()),
            working_dir: initial_dir,
            cols: config.window_width,
            rows: config.window_height,
            cell: (0, 0),
        };
        let probe = startup.clone();
        thread::Builder::new()
            .name("tern-first-shell".into())
            .spawn(move || {
                let pane = Pane::spawn(&config, launch, Arc::new(FrameRequest::default()));
                if let Some(probe) = probe {
                    probe.mark("shell started");
                }
                pane
            })
            .map_err(|e| format!("cannot start a thread: {e}"))?
    };

    // AppKit would otherwise treat command line arguments as documents to open.
    let defaults = NSDictionary::from_slices(
        &[ns_string!("NSTreatUnknownArgumentsAsOpen")],
        &[ns_string!("NO") as &AnyObject],
    );
    // SAFETY: a dictionary of property list values.
    unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&defaults) };

    let clipboard = Clipboard::new(options.pasteboard.as_deref());
    // App Nap would otherwise throttle the whole process, session threads
    // included, once its windows are covered: a build in a background
    // window would crawl. A terminal's programs are the user's work. Latency
    // critical as well: without it macOS still lowers every thread of a
    // hidden app below the default priority after a minute or so, and under
    // load its output then crawls just the same.
    let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::LatencyCritical,
        ns_string!("Programs in terminal windows keep running when the windows are hidden"),
    );
    let delegate = AppDelegate::new(mtm);
    let app = Rc::new(App {
        mtm,
        options,
        font_size: Cell::new(config.font_size),
        config: RefCell::new(Rc::new(config)),
        renderer: OnceCell::new(),
        renderer_setup: RefCell::new(Some(renderer_setup)),
        delegate,
        windows: RefCell::new(Vec::new()),
        next_id: Cell::new(1),
        focused: Cell::new(None),
        menu_commands: RefCell::new(Vec::new()),
        context_actions: RefCell::new(Vec::new()),
        startup,
        first_pane: RefCell::new(Some(first_pane)),
        restored_session: RefCell::new(restored),
        last_session: RefCell::new(None),
        clipboard,
        opened_links: RefCell::new(Vec::new()),
        notifications: RefCell::new(Vec::new()),
        last_bell: Cell::new(None),
        bells: Cell::new(0),
        control,
        quit_confirmed: Cell::new(false),
        questions: RefCell::new(Vec::new()),
        notices: Cell::new(0),
        renaming: RefCell::new(None),
        tab_drag: RefCell::new(None),
        tab_pointer: Cell::new(None),
        startup_problems: RefCell::new(diagnostics.iter().map(ToString::to_string).collect()),
        _activity: activity,
    });
    APP.with(|cell| cell.set(Rc::clone(&app)))
        .map_err(|_| "the application was started twice".to_string())?;

    let ns = NSApplication::sharedApplication(mtm);
    ns.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let commands = menu::install(mtm, &ns, &app.config().bindings, &app.delegate);
    *app.menu_commands.borrow_mut() = commands;
    ns.setDelegate(Some(ProtocolObject::from_ref(&*app.delegate)));
    // SAFETY: `app.delegate` lives inside `APP` for the lifetime of the process.
    unsafe { ns.setServicesProvider(Some(&*app.delegate)) };
    app.mark("AppKit set up");
    drop(app);
    ns.run();
    Ok(())
}

/// The directory a configured working-directory names; None means home.
fn configured_dir(config: &Config) -> Option<PathBuf> {
    match &config.working_directory {
        WorkingDirectory::Path(path) => Some(path.clone()),
        WorkingDirectory::Home | WorkingDirectory::Inherit => None,
    }
}

fn dir_for_path(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

impl App {
    /// The configuration as it is now; a reload replaces it.
    pub fn config(&self) -> Rc<Config> {
        Rc::clone(&self.config.borrow())
    }

    fn mark(&self, what: &str) {
        if let Some(probe) = &self.startup {
            probe.mark(what);
        }
    }

    /// The renderer, finished from the setup thread's work on first use.
    /// Without one nothing can be drawn, so failure ends the process.
    pub fn renderer(&self) -> RefMut<'_, Renderer> {
        self.renderer
            .get_or_init(|| RefCell::new(self.finish_renderer()))
            .borrow_mut()
    }

    fn finish_renderer(&self) -> Renderer {
        let setup = self.renderer_setup.take().map(JoinHandle::join);
        let prepared = match setup {
            Some(Ok((result, took))) => {
                if let Some(probe) = &self.startup {
                    probe.mark_span("renderer ready", took);
                }
                result
            }
            Some(Err(_)) => fail("the renderer setup thread panicked"),
            None => Renderer::prepare(&self.font()),
        };
        match prepared {
            Ok(prepared) => {
                let renderer = Renderer::from_prepared(prepared);
                if let Some(probe) = &self.startup {
                    let t = renderer.startup_times();
                    probe.mark_span("  of which Metal device", t.device);
                    probe.mark_span("  of which shader pipeline", t.pipeline);
                    probe.mark_span("  of which font lookup (in parallel)", t.font);
                }
                renderer
            }
            Err(RenderError::FontNotFound(family)) => {
                // Shown with the configuration's other problems once the
                // first window is up.
                self.startup_problems.borrow_mut().push(format!(
                    "font-family: no font family named \"{family}\"; using {}",
                    config::font_name("")
                ));
                // The empty name: the system's monospaced font.
                let system = FontConfig {
                    family: String::new(),
                    ..self.font()
                };
                // The font in use from now on, so size changes and reloads
                // start from it.
                let mut config = (*self.config()).clone();
                config.font_family.clone_from(&system.family);
                *self.config.borrow_mut() = Rc::new(config);
                Renderer::new(&system).unwrap_or_else(|e| fail(&e.to_string()))
            }
            Err(e) => fail(&e.to_string()),
        }
    }

    fn font(&self) -> FontConfig {
        FontConfig {
            family: self.config().font_family.clone(),
            size_pt: self.font_size.get(),
        }
    }

    /// Whether windows keep a log of their input, which only the
    /// automation socket reads.
    pub fn records_input(&self) -> bool {
        self.control.is_some()
    }

    pub fn font_size(&self) -> f32 {
        self.current_window()
            .and_then(|w| self.id_of(&w))
            .and_then(|id| self.state(id))
            .map_or_else(|| self.font_size.get(), |s| s.font_size())
    }
    fn did_finish_launching(&self) {
        self.mark("application launched");
        self.watch_pane_moves();
        let first = self.first_pane.take().map(JoinHandle::join);
        let pane = match first {
            Some(Ok(Ok(pane))) => pane,
            Some(Ok(Err(e))) => fail(&format!("cannot start the shell: {e}")),
            Some(Err(_)) => fail("the thread starting the shell panicked"),
            None => return,
        };
        // A window opened for a file at launch counts as the first window,
        // and the shell started for it is not needed.
        let restored = self.restored_session.take();
        if self.windows.borrow().is_empty() {
            if let Some(saved) = restored {
                self.restore_saved_session(saved, pane);
                if self.windows.borrow().is_empty() {
                    let dir = configured_dir(&self.config());
                    self.create_window(Placement::Window, None, dir);
                }
            } else {
                self.create_window(Placement::Window, Some(pane), None);
                if self.options.paths.len() > 1 {
                    self.open_paths(self.options.paths[1..].to_vec());
                }
            }
        }
        let problems = self.startup_problems.take();
        self.report_problems(&problems);
        if !self.options.no_activate {
            // Launched from a shell rather than by Launch Services, the app
            // is not activated for us.
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        }
    }

    /// Opens a window running pane, or a new shell in working_dir.
    pub fn create_window(
        &self,
        placement: Placement,
        pane: Option<Pane>,
        working_dir: Option<PathBuf>,
    ) {
        let config = self.config();
        let cascade_from = self.current_window();
        // A window of its own with none to cascade from opens where the
        // configuration says, and is made on that screen at its scale: moving
        // it there after costs tens of milliseconds, more when the scale changes.
        let screen = match (&placement, &cascade_from) {
            (Placement::Window, None) => self.first_window_screen(),
            _ => None,
        };
        let scale = match (&placement, &cascade_from, &screen) {
            (Placement::TabOf(parent), _, _) => parent.backingScaleFactor(),
            (_, Some(previous), _) => previous.backingScaleFactor(),
            (_, None, Some(screen)) => screen.backingScaleFactor(),
            (_, None, None) => 1.0,
        };
        let metrics = self.renderer().metrics(scale as f32);
        let pane = match pane {
            Some(pane) => pane,
            None => {
                let launch = Launch {
                    id: new_pane_id(),
                    command: None,
                    working_dir,
                    cols: config.window_width,
                    rows: config.window_height,
                    cell: crate::pane::cell_size(&metrics),
                };
                match Pane::spawn(&config, launch, Arc::new(FrameRequest::default())) {
                    Ok(pane) => pane,
                    Err(e) => {
                        eprintln!("tern: cannot start the shell: {e}");
                        return;
                    }
                }
            }
        };
        let events = Arc::clone(&pane.events);
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let first = id == 1;
        let size = self.content_size(scale);
        let origin = screen.as_ref().and_then(|_| self.configured_origin(size));
        let tern = TernWindow::new(
            self.mtm,
            id,
            NSRect::new(origin.unwrap_or(NSPoint::new(0.0, 0.0)), size),
            screen.as_deref().filter(|_| origin.is_some()),
            pane,
            metrics,
            self.startup.clone().filter(|_| first),
        );
        let window = tern.window.clone();
        self.windows.borrow_mut().push(tern);
        match placement {
            Placement::Window => {
                match origin {
                    // AppKit keeps a window made on a screen inside it; the
                    // setting may ask for a corner past the edge.
                    Some(origin) => window.setFrameOrigin(origin),
                    None => self.place(&window, cascade_from.as_deref()),
                }
                // The screen it landed on may have another scale, and so
                // other cell sizes.
                let landed = window.backingScaleFactor();
                if landed != scale {
                    let size = self.content_size(landed);
                    window.setContentSize(size);
                    match self
                        .configured_origin(size)
                        .filter(|_| cascade_from.is_none())
                    {
                        Some(origin) => window.setFrameOrigin(origin),
                        None => self.place(&window, cascade_from.as_deref()),
                    }
                }
                // Preferred tabbing would otherwise put the window into the
                // frontmost window's tab group.
                window.setTabbingMode(NSWindowTabbingMode::Disallowed);
                self.show(&window);
                window.setTabbingMode(NSWindowTabbingMode::Preferred);
            }
            Placement::TabOf(parent) => {
                parent.addTabbedWindow_ordered(&window, NSWindowOrderingMode::Above);
                self.show(&window);
                if let Some(tabs) = parent.tabbedWindows() {
                    for tab in tabs.iter() {
                        if let Some(tid) = self.id_of(&tab)
                            && let Some(st) = self.state(tid)
                        {
                            st.sync_tab_activity(true);
                        }
                    }
                }
                if let Some(st) = self.state(id) {
                    st.reapply_chrome();
                    crate::window::reapply_chrome_soon(id);
                }
            }
        }
        if first {
            self.mark("first window shown");
        }
        if let Some(state) = self.state(id) {
            state.draw_now();
        }
        // Anything the session reported before it had a window.
        self.pane_events(&events);
        // Questions that waited for a window to show on.
        self.ask_next();
    }

    /// The window that menu actions and automation commands act on.
    pub fn current_window(&self) -> Option<Retained<NSWindow>> {
        let ns = NSApplication::sharedApplication(self.mtm);
        let candidate = [ns.keyWindow(), ns.mainWindow()]
            .into_iter()
            .flatten()
            .find(|w| self.id_of(w).is_some())
            .or_else(|| {
                // While the app is inactive there is no key window.
                self.focused
                    .get()
                    .and_then(|id| self.window(id))
                    .filter(|w| w.isVisible())
            })
            .or_else(|| {
                ns.orderedWindows()
                    .iter()
                    .find(|w| self.id_of(w).is_some() && w.isVisible())
            })
            .or_else(|| self.windows.borrow().last().map(|t| t.window.clone()))?;
        // Selecting a tab changes the group's selection before key status follows.
        let selected = candidate
            .tabGroup()
            .and_then(|group| group.selectedWindow())
            .filter(|w| self.id_of(w).is_some());
        Some(selected.unwrap_or(candidate))
    }

    pub fn id_of(&self, window: &NSWindow) -> Option<u64> {
        let ptr: *const NSWindow = window;
        self.windows
            .borrow()
            .iter()
            .find(|t| std::ptr::eq(Retained::as_ptr(&t.window), ptr))
            .map(|t| t.state.id)
    }

    pub fn window(&self, id: u64) -> Option<Retained<NSWindow>> {
        self.windows
            .borrow()
            .iter()
            .find(|t| t.state.id == id)
            .map(|t| t.window.clone())
    }

    pub fn state(&self, id: u64) -> Option<Rc<WindowState>> {
        self.windows
            .borrow()
            .iter()
            .find(|t| t.state.id == id)
            .map(|t| Rc::clone(&t.state))
    }

    pub fn view_of(&self, window: &NSWindow) -> Option<Retained<TernView>> {
        let ptr: *const NSWindow = window;
        self.windows
            .borrow()
            .iter()
            .find(|t| std::ptr::eq(Retained::as_ptr(&t.window), ptr))
            .map(|t| t.view.clone())
    }

    /// The window holding a pane.
    fn state_with_pane(&self, pane: PaneId) -> Option<Rc<WindowState>> {
        self.windows
            .borrow()
            .iter()
            .find(|t| t.state.contains_pane(pane))
            .map(|t| Rc::clone(&t.state))
    }

    /// Every open window, oldest first.
    pub fn all_windows(&self) -> Vec<(Rc<WindowState>, Retained<NSWindow>, Retained<TernView>)> {
        self.windows
            .borrow()
            .iter()
            .map(|t| (Rc::clone(&t.state), t.window.clone(), t.view.clone()))
            .collect()
    }

    pub fn window_will_close(&self, id: u64) {
        self.remember_closing_last_window();
        self.rename_window_closing(id);
        let closed = {
            let mut windows = self.windows.borrow_mut();
            let i = windows.iter().position(|t| t.state.id == id);
            i.map(|i| windows.remove(i))
        };
        if let Some(closed) = closed {
            closed.retire();
        }
        if self.focused.get() == Some(id) {
            self.focused.set(None);
        }
        // A sheet that was on it went with it; once AppKit has moved key
        // status on, its question finds another window.
        post_to_main(true, |_| app().ask_next());
    }

    pub fn window_became_key(&self, id: u64) {
        // The tab bar a name was being typed in is no longer the one in front.
        self.rename_displaced(id, true);
        self.focused.set(Some(id));
        if let Some(state) = self.state(id) {
            state.set_focused(true);
        }
    }

    pub fn new_tab_next_to(&self, id: u64) {
        let placement = self.window(id).map_or(Placement::Window, Placement::TabOf);
        let dir = self.new_pane_dir(self.state(id).as_deref());
        self.create_window(placement, None, dir);
    }

    /// Where a new tab or window opened from this window starts.
    fn new_pane_dir(&self, from: Option<&WindowState>) -> Option<PathBuf> {
        match &self.config().working_directory {
            WorkingDirectory::Inherit => {
                from.and_then(|s| s.with_focused(|p| p.session.cwd()).flatten())
            }
            _ => configured_dir(&self.config()),
        }
    }

    /// Files and folders opened with the app (dropped on its Dock icon,
    /// or Open With in the Finder): a window in each folder, or in the
    /// folder that holds each file.
    pub fn open_paths(&self, paths: Vec<PathBuf>) {
        for path in paths {
            self.create_window(Placement::Window, None, dir_for_path(&path));
        }
    }

    pub fn open_paths_in_tabs(&self, paths: Vec<PathBuf>) {
        for path in paths {
            let placement = self
                .current_window()
                .map_or(Placement::Window, Placement::TabOf);
            self.create_window(placement, None, dir_for_path(&path));
        }
    }

    /// The Dock icon was clicked: with no windows at all, one opens. True,
    /// so AppKit restores a minimized window itself (those exist but are
    /// not visible).
    pub fn reopen(&self, has_visible_windows: bool) -> bool {
        if !has_visible_windows && self.windows.borrow().is_empty() {
            let dir = configured_dir(&self.config());
            self.create_window(Placement::Window, None, dir);
        }
        true
    }

    // ---- session events ----

    /// A session asked for a frame while its window's display link was stopped.
    pub fn pane_woke(&self, pane: PaneId) {
        if let Some(state) = self.state_with_pane(pane) {
            // The wakeup flagged its pane; the others need no snapshot.
            state.woke();
        }
    }

    /// Titles, bells, clipboard writes and exits a session reported.
    pub fn pane_events(&self, events: &Arc<PaneEvents>) {
        let Some(state) = self.state_with_pane(events.pane) else {
            // Not in a window yet; create_window delivers them.
            return;
        };
        let pending = events.take();
        if let Some(title) = pending.title {
            state.set_pane_title(events.pane, title);
        }
        if pending.bell {
            state.mark_tab_attention();
            self.bell();
        }
        for (title, body) in pending.notifications {
            state.mark_tab_attention();
            self.pane_notify(events.pane, title, body);
        }
        if let Some(progress) = pending.progress {
            state.set_pane_progress(events.pane, progress);
        }
        if let Some(text) = pending.clipboard {
            self.clipboard.set_text(&text);
        }
        if let Some(status) = pending.exited
            && !state.hold_after_exit(events.pane, status) {
                self.close_pane(&state, events.pane);
            }
    }

    fn bell(&self) {
        let now = Instant::now();
        if !bell_due(self.last_bell.get(), now) {
            return;
        }
        self.last_bell.set(Some(now));
        self.bells.set(self.bells.get() + 1);
        NSBeep();
    }

    pub fn bells(&self) -> u64 {
        self.bells.get()
    }

    // ---- the clipboard ----

    pub fn clipboard(&self) -> &Clipboard {
        &self.clipboard
    }

    fn will_terminate(&self) {
        self.persist_session_on_quit();
        if let Some(control) = &self.control {
            control.remove();
        }
        self.clipboard.release();
    }
}

/// Whether the bell may sound now, having last sounded at last.
fn bell_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= BELL_INTERVAL)
}

fn fail(message: &str) -> ! {
    eprintln!("tern: {message}");
    std::process::exit(1)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type has no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernAppDelegate"]
    struct AppDelegate;

    // SAFETY: NSObjectProtocol has no requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: the methods below match the protocol's signatures.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _: &NSNotification) {
            app().did_finish_launching();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _: &NSApplication) -> bool {
            // Run as "tern -e command" from a shell, the app is that
            // command: when it is done, so is the app, or the shell that
            // started it would seem to hang.
            let app = app();
            app.config().quit_after_last_window_closed || !app.options.command.is_empty()
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _: &NSApplication, has_visible_windows: bool) -> bool {
            app().reopen(has_visible_windows)
        }

        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(&self, _: &NSApplication, urls: &NSArray<NSURL>) {
            let paths = urls
                .iter()
                .filter(|url| url.isFileURL())
                .filter_map(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()))
                .collect();
            app().open_paths(paths);
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(&self, _: &NSApplication) -> NSApplicationTerminateReply {
            app().should_terminate()
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _: &NSNotification) {
            app().will_terminate();
        }

        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        fn supports_secure_restorable_state(&self, _: &NSApplication) -> bool {
            true
        }
    }

    // SAFETY: validateMenuItem: takes the item and returns BOOL.
    unsafe impl NSMenuItemValidation for AppDelegate {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            app().validate_menu_item(item.tag())
        }
    }

    impl AppDelegate {
        /// The action of every menu item; the item's tag says which command.
        #[unsafe(method(performAction:))]
        fn perform_action(&self, sender: Option<&AnyObject>) {
            if let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) {
                app().perform_menu_item(item.tag());
            }
        }

        /// The action of every right-click context menu item.
        #[unsafe(method(performContextAction:))]
        fn perform_context_action(&self, sender: Option<&AnyObject>) {
            if let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) {
                app().perform_context_action(item.tag());
            }
        }

        /// Finder Service: "New Tern Window Here".
        #[unsafe(method(openWindowHere:userData:error:))]
        fn open_window_here(
            &self,
            pboard: &NSPasteboard,
            _user_data: Option<&NSString>,
            _error: *mut *mut NSString,
        ) {
            if let Some(crate::input::DropPayload::Paths(paths)) =
                crate::clipboard::read_drop(pboard)
            {
                app().open_paths(paths);
            }
        }

        /// Finder Service: "New Tern Tab Here".
        #[unsafe(method(openTabHere:userData:error:))]
        fn open_tab_here(
            &self,
            pboard: &NSPasteboard,
            _user_data: Option<&NSString>,
            _error: *mut *mut NSString,
        ) {
            if let Some(crate::input::DropPayload::Paths(paths)) =
                crate::clipboard::read_drop(pboard)
            {
                app().open_paths_in_tabs(paths);
            }
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<AppDelegate> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bell_sounds_at_most_once_a_second() {
        let t = Instant::now();
        assert!(bell_due(None, t));
        assert!(!bell_due(Some(t), t));
        assert!(!bell_due(Some(t), t + Duration::from_millis(999)));
        assert!(bell_due(Some(t), t + BELL_INTERVAL));
    }

    #[test]
    fn working_directory_setting() {
        let mut config = Config::default();
        assert_eq!(configured_dir(&config), None);
        config.working_directory = WorkingDirectory::Path("/tmp".into());
        assert_eq!(configured_dir(&config), Some(PathBuf::from("/tmp")));
        config.working_directory = WorkingDirectory::Home;
        assert_eq!(configured_dir(&config), None);
    }
}
