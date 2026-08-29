use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, FontWeight, Hsla, MouseButton, PathBuilder,
    PathPromptOptions, PromptButton, PromptLevel, Render, SharedString, Subscription, Window,
    WindowAppearance, canvas, div, point, prelude::*, px, rgb,
};

use crate::{
    CloseWorkspace, NewWorkspace, OpenFolder, OpenSettings, SelectWorkspace,
    editor::{Editor, EditorEvent},
};

const MAX_RECENTS: usize = 8;
const CONTENT_WIDTH: f32 = 480.0;
const REPOSITORY_URL: &str = "https://github.com/arjunkomath/Nuvi";
const NEW_ISSUE_URL: &str = "https://github.com/arjunkomath/Nuvi/issues/new";

#[derive(Clone, Copy)]
struct Theme {
    chrome: u32,
    chrome_opacity: f32,
    panel: u32,
    raised: u32,
    border: u32,
    text: u32,
    muted: u32,
    accent: u32,
    error: u32,
}

impl Theme {
    fn for_appearance(appearance: WindowAppearance) -> Self {
        if matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ) {
            Self {
                chrome: 0x191b1e,
                chrome_opacity: 0.78,
                panel: 0x222529,
                raised: 0x2d3035,
                border: 0x3a3d42,
                text: 0xe7e4de,
                muted: 0x92908b,
                accent: 0x6ea8fe,
                error: 0xd58a7b,
            }
        } else {
            Self {
                chrome: 0xf2f1ef,
                chrome_opacity: 0.82,
                panel: 0xf8f7f5,
                raised: 0xffffff,
                border: 0xdedbd6,
                text: 0x343330,
                muted: 0x77746f,
                accent: 0x1769c2,
                error: 0xa94c40,
            }
        }
    }
}

#[derive(Clone)]
enum TabContent {
    Launcher,
    Settings,
    Editor(Entity<Editor>),
}

struct WorkspaceTab {
    id: usize,
    title: SharedString,
    /// Title reported by Neovim's `set_title` event, shown on the window
    /// while this tab is active; the tab label keeps the workspace name.
    window_title: Option<SharedString>,
    content: TabContent,
    /// Dropped with the tab, unsubscribing from its editor's events.
    _editor_subscription: Option<Subscription>,
}

#[derive(Clone, Copy)]
enum TitlebarIcon {
    Add,
    Close,
}

pub struct WorkspaceWindow {
    focus: FocusHandle,
    tabs: Vec<WorkspaceTab>,
    active: usize,
    next_id: usize,
    recents: Vec<PathBuf>,
    status: Option<SharedString>,
    closing_window: bool,
    confirming_window_close: bool,
    allow_window_close: bool,
    subscriptions: Vec<Subscription>,
}

impl WorkspaceWindow {
    pub fn new(window: &mut Window, args: Vec<OsString>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            recents: load_recents(),
            status: None,
            closing_window: false,
            confirming_window_close: false,
            allow_window_close: false,
            subscriptions: Vec::new(),
        };

        if args.is_empty() {
            this.push_launcher();
            this.focus.focus(window);
        } else {
            let path = args
                .iter()
                .rev()
                .map(PathBuf::from)
                .find(|path| path.is_dir())
                .and_then(|path| path.canonicalize().ok().or(Some(path)));
            this.push_editor(args, path.clone(), path.clone(), false, window, cx);
            if let Some(path) = path {
                promote_recent(&mut this.recents, path, MAX_RECENTS);
                this.save_recents();
            }
        }
        let appearance = cx.observe_window_appearance(window, |_, _, cx| cx.notify());
        this.subscriptions.push(appearance);
        this
    }

    pub fn bind_window(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let weak = this.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.request_window_close(window, cx))
                .unwrap_or(true)
        });
    }

    pub fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_window = false;
        self.deactivate_current(cx);
        self.push_launcher();
        self.active = self.tabs.len() - 1;
        self.focus.focus(window);
        window.set_window_title("Nuvi");
        cx.notify();
    }

    pub fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_window = false;
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Workspace".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = selected.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ =
                    this.update_in(cx, |this, window, cx| this.open_workspace(path, window, cx));
            }
        })
        .detach();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_window = false;
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| matches!(tab.content, TabContent::Settings))
        {
            self.activate(index, window, cx);
            return;
        }

        let id = self.take_id();
        self.tabs.push(WorkspaceTab {
            id,
            title: "Settings".into(),
            window_title: None,
            content: TabContent::Settings,
            _editor_subscription: None,
        });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    pub fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        match &tab.content {
            TabContent::Launcher => {
                if self.tabs.len() > 1 {
                    self.remove_tab(self.active, window, cx);
                }
            }
            TabContent::Settings => self.remove_tab(self.active, window, cx),
            TabContent::Editor(editor) => {
                if editor.update(cx, |editor, _| editor.request_close()) {
                    self.remove_tab(self.active, window, cx);
                }
            }
        }
    }

    pub fn request_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.allow_window_close {
            return true;
        }
        if self.confirming_window_close {
            return false;
        }

        self.confirming_window_close = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Quit Nuvi?",
            Some("All open workspaces will be closed."),
            &[PromptButton::cancel("Cancel"), PromptButton::ok("Quit")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await.ok() == Some(1);
            let _ = this.update_in(cx, |this, window, cx| {
                this.confirming_window_close = false;
                if confirmed && this.begin_window_close(window, cx) {
                    window.remove_window();
                }
            });
        })
        .detach();
        false
    }

    fn begin_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.closing_window = true;
        self.close_next_editor(window, cx)
    }

    fn push_launcher(&mut self) {
        let id = self.take_id();
        self.tabs.push(WorkspaceTab {
            id,
            title: "New Workspace".into(),
            window_title: None,
            content: TabContent::Launcher,
            _editor_subscription: None,
        });
    }

    fn open_workspace(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_window = false;
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            self.status = Some("That folder is no longer available.".into());
            self.recents.retain(|recent| recent != &path);
            self.save_recents();
            cx.notify();
            return;
        }

        self.deactivate_current(cx);
        let replace = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| matches!(tab.content, TabContent::Launcher));
        self.push_editor(
            vec![path.clone().into_os_string()],
            Some(path.clone()),
            Some(path.clone()),
            replace,
            window,
            cx,
        );
        promote_recent(&mut self.recents, path, MAX_RECENTS);
        self.save_recents();
        self.status = None;
    }

    fn push_editor(
        &mut self,
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
        path: Option<PathBuf>,
        replace_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.take_id();
        let title = path
            .as_deref()
            .map(workspace_name)
            .unwrap_or("Workspace")
            .to_string();
        let editor = cx.new(|cx| Editor::new(window, args, working_directory, cx));
        Editor::bind_window(&editor, window, cx);
        let subscription =
            cx.subscribe_in(
                &editor,
                window,
                move |this, _, event, window, cx| match event {
                    EditorEvent::CloseCancelled => this.closing_window = false,
                    EditorEvent::Closed => this.editor_closed(id, window, cx),
                    EditorEvent::Title(title) => {
                        this.editor_title_changed(id, title.clone(), window)
                    }
                },
            );

        let tab = WorkspaceTab {
            id,
            title: title.into(),
            window_title: None,
            content: TabContent::Editor(editor),
            _editor_subscription: Some(subscription),
        };
        if replace_active {
            self.tabs[self.active] = tab;
        } else {
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.activate(self.active, window, cx);
    }

    fn editor_title_changed(&mut self, id: usize, title: SharedString, window: &mut Window) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.tabs[index].window_title = Some(title.clone());
        if index == self.active {
            window.set_window_title(&title);
        }
    }

    fn editor_closed(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        self.remove_tab(index, window, cx);

        if self.closing_window && self.close_next_editor(window, cx) {
            window.remove_window();
        }
    }

    fn close_next_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        loop {
            let Some(index) = self
                .tabs
                .iter()
                .position(|tab| matches!(tab.content, TabContent::Editor(_)))
            else {
                self.allow_window_close = true;
                return true;
            };
            self.activate(index, window, cx);
            let TabContent::Editor(editor) = &self.tabs[index].content else {
                unreachable!();
            };
            if !editor.update(cx, |editor, _| editor.request_close()) {
                return false;
            }
            self.remove_tab(index, window, cx);
        }
    }

    fn remove_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.remove(index);
        if self.tabs.is_empty() && !self.closing_window {
            self.push_launcher();
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        if !self.tabs.is_empty() {
            self.activate(self.active, window, cx);
        }
        cx.notify();
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.deactivate_current(cx);
        self.active = index;
        let tab = &self.tabs[index];
        window.set_window_title(tab.window_title.as_deref().unwrap_or(&tab.title));
        match &tab.content {
            TabContent::Launcher | TabContent::Settings => self.focus.focus(window),
            TabContent::Editor(editor) => {
                editor.update(cx, |editor, _| editor.activate(window));
            }
        }
        cx.notify();
    }

    fn select_workspace(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_window = false;
        self.activate(index, window, cx);
    }

    fn deactivate_current(&self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab {
            content: TabContent::Editor(editor),
            ..
        }) = self.tabs.get(self.active)
        {
            editor.update(cx, |editor, _| editor.deactivate());
        }
    }

    fn take_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn save_recents(&mut self) {
        if let Err(error) = write_recents(&self.recents) {
            self.status = Some(format!("Could not save recents: {error}").into());
        }
    }

    fn render_titlebar(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div()
            .flex()
            .min_w(px(0.0))
            .h_full()
            .items_center()
            .gap(px(4.0))
            .overflow_hidden();

        for (index, tab) in self.tabs.iter().enumerate() {
            let active = index == self.active;
            let id = tab.id;
            let title = tab.title.clone();
            let tab_background = if active {
                translucent(theme.raised, 0.62)
            } else {
                translucent(theme.chrome, 0.0)
            };
            let tab_border = if active {
                translucent(theme.border, 0.7)
            } else {
                translucent(theme.border, 0.4)
            };
            tabs = tabs.child(
                div()
                    .id(("workspace-tab", id))
                    .h(px(32.0))
                    .w(px(200.0))
                    .min_w(px(110.0))
                    .max_w(px(200.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .rounded(px(16.0))
                    .border_1()
                    .border_color(tab_border)
                    .bg(tab_background)
                    .text_color(rgb(if active { theme.text } else { theme.muted }))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .hover(move |this| this.bg(translucent(theme.raised, 0.72)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.closing_window = false;
                        this.activate(index, window, cx);
                    }))
                    .child(div().min_w(px(0.0)).flex_1().truncate().child(title))
                    .child(
                        div()
                            .id(("close-workspace", id))
                            .flex_none()
                            .size(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.0))
                            .text_color(rgb(theme.muted))
                            .hover(move |this| {
                                this.bg(rgb(theme.border)).text_color(rgb(theme.text))
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                                    this.activate(index, window, cx);
                                    this.close_workspace(window, cx);
                                }
                            }))
                            .child(titlebar_icon(TitlebarIcon::Close)),
                    ),
            );
        }

        div()
            .h(px(46.0))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(7.0))
            .pl(px(88.0))
            .pr(px(12.0))
            .on_mouse_down(MouseButton::Left, |event, window, cx| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
                cx.stop_propagation();
            })
            .child(tabs)
            .child(
                div()
                    .id("new-workspace")
                    .size(px(28.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.0))
                    .text_color(rgb(theme.muted))
                    .cursor_pointer()
                    .hover(move |this| this.bg(rgb(theme.raised)).text_color(rgb(theme.text)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.new_workspace(window, cx)))
                    .child(titlebar_icon(TitlebarIcon::Add)),
            )
    }

    fn render_launcher(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut recent_items = div()
            .mt(px(8.0))
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(translucent(theme.raised, 0.45));

        if self.recents.is_empty() {
            recent_items = recent_items.child(
                div()
                    .h(px(48.0))
                    .flex()
                    .items_center()
                    .px(px(14.0))
                    .text_size(px(13.0))
                    .text_color(rgb(theme.muted))
                    .child("Folders you open will appear here."),
            );
        } else {
            for (index, path) in self.recents.iter().enumerate() {
                let selected = path.clone();
                let name = workspace_name(path).to_string();
                let parent = display_parent(path);
                recent_items = recent_items.child(
                    div()
                        .id(("recent-workspace", index))
                        .h(px(52.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .px(px(14.0))
                        .when(index + 1 < self.recents.len(), |row| {
                            row.border_b_1().border_color(rgb(theme.border))
                        })
                        .cursor_pointer()
                        .hover(move |this| this.bg(translucent(theme.raised, 0.75)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_workspace(selected.clone(), window, cx)
                        }))
                        .child(
                            div()
                                .min_w(px(0.0))
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(theme.text))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(11.0))
                                        .text_color(rgb(theme.muted))
                                        .child(parent),
                                ),
                        ),
                );
            }
        }

        let recents = div()
            .mt(px(20.0))
            .w_full()
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(theme.muted))
                    .child("Recent Projects"),
            )
            .child(recent_items);

        div()
            .relative()
            .size_full()
            .flex()
            .justify_center()
            .text_color(rgb(theme.text))
            .child(
                div()
                    .w(px(CONTENT_WIDTH))
                    .max_w_full()
                    .px(px(22.0))
                    .pt(px(54.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(px(28.0))
                            .child("Open a workspace"),
                    )
                    .child(
                        div().mt(px(8.0)).flex().child(
                            div()
                                .id("open-folder")
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(theme.accent))
                                .cursor_pointer()
                                .hover(|this| this.underline())
                                .active(|this| this.opacity(0.82))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.choose_folder(window, cx)
                                    }),
                                )
                                .child("Choose a folder…"),
                        ),
                    )
                    .child(recents)
                    .when_some(self.status.clone(), |view, status| {
                        view.child(
                            div()
                                .mt(px(18.0))
                                .text_size(px(12.0))
                                .text_color(rgb(theme.error))
                                .child(status),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(18.0))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_size(px(11.0))
                    .text_color(translucent(theme.muted, 0.7))
                    .child(concat!("Nuvi v", env!("CARGO_PKG_VERSION"))),
            )
    }

    fn render_settings(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut shortcuts = div()
            .mt(px(8.0))
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(translucent(theme.raised, 0.45));
        let commands = [
            ("New Workspace", "⌘T"),
            ("Open Folder", "⌘O"),
            ("Close Tab", "⌘W"),
            ("Open Settings", "⌘,"),
            ("Select Tab 1–9", "⌘1–9"),
            ("Quit Nuvi", "⌘Q"),
        ];
        for (index, (label, shortcut)) in commands.into_iter().enumerate() {
            shortcuts = shortcuts.child(
                div()
                    .h(px(42.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .when(index + 1 < commands.len(), |row| {
                        row.border_b_1().border_color(rgb(theme.border))
                    })
                    .child(div().text_size(px(13.0)).child(label))
                    .child(
                        div()
                            .px(px(8.0))
                            .py(px(3.0))
                            .rounded(px(6.0))
                            .bg(translucent(theme.border, 0.35))
                            .text_size(px(11.0))
                            .text_color(rgb(theme.muted))
                            .child(shortcut),
                    ),
            );
        }

        let about = div()
            .mt(px(8.0))
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(translucent(theme.raised, 0.45))
            .child(
                div()
                    .h(px(42.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(div().text_size(px(13.0)).child("Name"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.muted))
                            .child("Nuvi"),
                    ),
            )
            .child(
                div()
                    .h(px(42.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(div().text_size(px(13.0)).child("Version"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.muted))
                            .child(env!("CARGO_PKG_VERSION")),
                    ),
            )
            .child(
                div()
                    .h(px(42.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .child(div().text_size(px(13.0)).child("Links"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(16.0))
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .id("settings-github")
                                    .text_color(rgb(theme.accent))
                                    .cursor_pointer()
                                    .hover(|link| link.underline())
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.open_url(REPOSITORY_URL)),
                                    )
                                    .child("GitHub"),
                            )
                            .child(
                                div()
                                    .id("settings-report-issue")
                                    .text_color(rgb(theme.accent))
                                    .cursor_pointer()
                                    .hover(|link| link.underline())
                                    .on_click(cx.listener(|_, _, _, cx| cx.open_url(NEW_ISSUE_URL)))
                                    .child("Report an issue"),
                            ),
                    ),
            );

        div()
            .size_full()
            .flex()
            .justify_center()
            .text_color(rgb(theme.text))
            .child(
                div()
                    .w(px(CONTENT_WIDTH))
                    .max_w_full()
                    .px(px(22.0))
                    .py(px(46.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(px(28.0))
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .mt(px(26.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(theme.muted))
                            .child("Keyboard Shortcuts"),
                    )
                    .child(shortcuts)
                    .child(
                        div()
                            .mt(px(28.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(theme.muted))
                            .child("About"),
                    )
                    .child(about),
            )
    }
}

fn titlebar_icon(icon: TitlebarIcon) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let offset = match icon {
                TitlebarIcon::Add => px(5.0),
                TitlebarIcon::Close => px(4.0),
            };
            let mut path = PathBuilder::stroke(px(2.0));
            match icon {
                TitlebarIcon::Add => {
                    path.move_to(point(center.x - offset, center.y));
                    path.line_to(point(center.x + offset, center.y));
                    path.move_to(point(center.x, center.y - offset));
                    path.line_to(point(center.x, center.y + offset));
                }
                TitlebarIcon::Close => {
                    path.move_to(point(center.x - offset, center.y - offset));
                    path.line_to(point(center.x + offset, center.y + offset));
                    path.move_to(point(center.x + offset, center.y - offset));
                    path.line_to(point(center.x - offset, center.y + offset));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, window.text_style().color);
            }
        },
    )
    .size(px(16.0))
}

impl Render for WorkspaceWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_appearance(window.appearance());
        let content = self.tabs.get(self.active).map(|tab| tab.content.clone());
        div()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(translucent(theme.chrome, theme.chrome_opacity))
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                cx.stop_propagation();
                this.new_workspace(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseWorkspace, window, cx| {
                cx.stop_propagation();
                this.close_workspace(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                cx.stop_propagation();
                this.choose_folder(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                cx.stop_propagation();
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, action: &SelectWorkspace, window, cx| {
                this.select_workspace(action.index, window, cx);
            }))
            .child(self.render_titlebar(theme, cx))
            .child(
                div().min_h(px(0.0)).flex_1().px(px(6.0)).pb(px(6.0)).child(
                    div()
                        .relative()
                        .size_full()
                        .overflow_hidden()
                        .rounded(px(9.0))
                        .border_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.panel))
                        .when_some(content, |shell, content| match content {
                            TabContent::Launcher => shell.child(self.render_launcher(theme, cx)),
                            TabContent::Settings => shell.child(self.render_settings(theme, cx)),
                            TabContent::Editor(editor) => shell.child(editor),
                        }),
                ),
            )
    }
}

fn workspace_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Workspace")
}

fn translucent(value: u32, opacity: f32) -> Hsla {
    Hsla::from(rgb(value)).opacity(opacity)
}

fn display_parent(path: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(relative) = parent.strip_prefix(PathBuf::from(home))
    {
        return if relative.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", relative.display())
        };
    }
    parent.display().to_string()
}

fn recents_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/Nuvi")
            .join("recent-workspaces")
    })
}

fn load_recents() -> Vec<PathBuf> {
    let Some(file) = recents_file() else {
        return Vec::new();
    };
    std::fs::read_to_string(file)
        .map(|contents| {
            contents
                .lines()
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
                .take(MAX_RECENTS)
                .collect()
        })
        .unwrap_or_default()
}

fn write_recents(recents: &[PathBuf]) -> std::io::Result<()> {
    let Some(file) = recents_file() else {
        return Ok(());
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = recents
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(file, contents)
}

fn promote_recent(recents: &mut Vec<PathBuf>, path: PathBuf, limit: usize) {
    recents.retain(|recent| recent != &path);
    recents.insert(0, path);
    recents.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_workspaces_are_unique_newest_first_and_bounded() {
        let mut recents = vec!["/one".into(), "/two".into(), "/three".into()];
        promote_recent(&mut recents, "/two".into(), 3);
        promote_recent(&mut recents, "/four".into(), 3);
        assert_eq!(
            recents,
            vec![
                PathBuf::from("/four"),
                PathBuf::from("/two"),
                PathBuf::from("/one")
            ]
        );
    }
}
