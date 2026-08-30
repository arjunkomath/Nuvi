use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, Hsla, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PathPromptOptions, Pixels,
    PromptButton, PromptLevel, Render, SharedString, Subscription, Window, WindowAppearance,
    canvas, div, fill, point, prelude::*, px, rgb, size,
};

use crate::{
    CloseWorkspace, NewWorkspace, OpenFolder, OpenSettings, SelectWorkspace,
    editor::{Editor, EditorEvent},
};

const MAX_RECENTS: usize = 8;
const CONTENT_WIDTH: f32 = 480.0;
/// Width of the frame around the content panel, below the titlebar.
const FRAME_WIDTH: f32 = 6.0;
/// Corner radius of the content panel. The editor rounds itself one pixel
/// tighter (`EDITOR_CORNER_RADIUS`) to sit inside the panel's 1px border.
const PANEL_RADIUS: f32 = 9.0;
/// Outer radius of the frame's bottom corners; its inner curve
/// (`FRAME_RADIUS - FRAME_WIDTH`) then matches the panel's corners exactly.
const FRAME_RADIUS: f32 = PANEL_RADIUS + FRAME_WIDTH;
const DEFAULT_EDITOR_TRANSPARENCY: f32 = 0.0;
const REPOSITORY_URL: &str = "https://github.com/arjunkomath/Nuvi";
const NEW_ISSUE_URL: &str = "https://github.com/arjunkomath/Nuvi/issues/new";

#[derive(Clone, Copy)]
struct Theme {
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
        Self::for_dark(matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ))
    }

    fn for_dark(dark: bool) -> Self {
        if dark {
            Self {
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
    editor_transparency: f32,
    adjusting_editor_transparency: bool,
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
            editor_transparency: load_editor_transparency(),
            adjusting_editor_transparency: false,
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
        if self.confirming_window_close || self.closing_window {
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
        let theme = Theme::for_appearance(window.appearance());
        let default_colors = (theme.text, theme.panel, theme.text);
        let background_opacity = 1.0 - self.editor_transparency;
        let editor = cx.new(|cx| {
            Editor::new(
                window,
                args,
                working_directory,
                default_colors,
                background_opacity,
                cx,
            )
        });
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

    fn deactivate_current(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active)
            && let TabContent::Editor(editor) = &tab.content
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

    fn set_editor_transparency(
        &mut self,
        position: Pixels,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.editor_transparency = ((position - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        let opacity = 1.0 - self.editor_transparency;
        for tab in &self.tabs {
            if let TabContent::Editor(editor) = &tab.content {
                editor.update(cx, |editor, cx| editor.set_background_opacity(opacity, cx));
            }
        }
        cx.notify();
    }

    fn save_editor_transparency(&mut self) {
        if let Err(error) = write_editor_transparency(self.editor_transparency) {
            self.status = Some(format!("Could not save background transparency: {error}").into());
        }
    }

    fn render_titlebar(
        &self,
        theme: Theme,
        frame_opacity: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
            let hover_group: SharedString = format!("workspace-tab-{id}").into();
            let separator_after = !active
                && self
                    .tabs
                    .get(index + 1)
                    .is_some_and(|_| index + 1 != self.active);
            tabs = tabs.child(
                div()
                    .id(("workspace-tab", id))
                    .group(hover_group.clone())
                    .relative()
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
                    .border_color(translucent(theme.border, if active { 0.7 } else { 0.0 }))
                    .when(active, |tab| tab.bg(translucent(theme.panel, 0.9)))
                    .when(!active, |tab| {
                        tab.hover(move |tab| tab.bg(translucent(theme.raised, 0.72)))
                    })
                    .text_color(rgb(if active { theme.text } else { theme.muted }))
                    .text_size(px(12.0))
                    .cursor_pointer()
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
                            .when(!active, |close| {
                                close
                                    .invisible()
                                    .group_hover(hover_group, |style| style.visible())
                            })
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
                    )
                    .when(separator_after, |tab| {
                        tab.child(
                            div()
                                .absolute()
                                .top(px(7.0))
                                .right(px(-3.0))
                                .w(px(1.0))
                                .h(px(18.0))
                                .bg(translucent(theme.border, 0.7)),
                        )
                    }),
            );
        }

        div()
            .h(px(46.0))
            .w_full()
            .flex_none()
            .bg(translucent(theme.panel, frame_opacity))
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
        let editor_transparency = self.editor_transparency;
        let percentage = (editor_transparency * 100.0).round() as u8;
        let workspace = cx.entity();
        let slider = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let track = Bounds::new(
                    point(bounds.left() + px(6.0), bounds.center().y - px(2.0)),
                    size(bounds.size.width - px(12.0), px(4.0)),
                );
                window.on_mouse_event({
                    let workspace = workspace.clone();
                    move |event: &MouseDownEvent, _, _, cx| {
                        if event.button == MouseButton::Left && bounds.contains(&event.position) {
                            workspace.update(cx, |this, cx| {
                                this.adjusting_editor_transparency = true;
                                this.set_editor_transparency(event.position.x, track, cx);
                            });
                        }
                    }
                });
                window.on_mouse_event({
                    let workspace = workspace.clone();
                    move |event: &MouseMoveEvent, _, _, cx| {
                        if event.dragging() && workspace.read(cx).adjusting_editor_transparency {
                            workspace.update(cx, |this, cx| {
                                this.set_editor_transparency(event.position.x, track, cx);
                            });
                        }
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, _, _, cx| {
                    if event.button == MouseButton::Left
                        && workspace.read(cx).adjusting_editor_transparency
                    {
                        workspace.update(cx, |this, cx| {
                            this.adjusting_editor_transparency = false;
                            this.save_editor_transparency();
                            cx.notify();
                        });
                    }
                });

                let filled = Bounds::new(
                    track.origin,
                    size(track.size.width * editor_transparency, track.size.height),
                );
                let thumb_center = point(
                    track.left() + track.size.width * editor_transparency,
                    bounds.center().y,
                );
                window.paint_quad(fill(track, rgb(theme.border)).corner_radii(px(2.0)));
                window.paint_quad(fill(filled, rgb(theme.accent)).corner_radii(px(2.0)));
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(thumb_center.x - px(6.0), thumb_center.y - px(6.0)),
                            size(px(12.0), px(12.0)),
                        ),
                        rgb(theme.accent),
                    )
                    .corner_radii(px(6.0)),
                );
            },
        )
        .w(px(160.0))
        .h(px(20.0))
        .cursor_pointer();
        let appearance = div()
            .mt(px(8.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(translucent(theme.raised, 0.45))
            .child(
                div()
                    .h(px(58.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(20.0))
                    .px(px(14.0))
                    .child(div().text_size(px(13.0)).child("Background Transparency"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(slider)
                            .child(
                                div()
                                    .w(px(36.0))
                                    .text_right()
                                    .text_size(px(12.0))
                                    .text_color(rgb(theme.muted))
                                    .child(format!("{percentage}%")),
                            ),
                    ),
            );

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
                    .child(div().text_size(px(13.0)).child("Version"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(theme.muted))
                            .child(concat!("Nuvi ", env!("CARGO_PKG_VERSION"))),
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
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
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
                            .child("Appearance"),
                    )
                    .child(appearance)
                    .when_some(self.status.clone(), |view, status| {
                        view.child(
                            div()
                                .mt(px(10.0))
                                .text_size(px(12.0))
                                .text_color(rgb(theme.error))
                                .child(status),
                        )
                    })
                    .child(
                        div()
                            .mt(px(28.0))
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
        let frame_opacity = (1.0 - self.editor_transparency - 0.25).max(0.0);
        let content = self.tabs.get(self.active).map(|tab| tab.content.clone());
        div()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
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
            .child(self.render_titlebar(theme, frame_opacity, cx))
            .child(
                div()
                    .relative()
                    .min_h(px(0.0))
                    .flex_1()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let frame_color = translucent(theme.panel, frame_opacity);
                                window.paint_quad(
                                    fill(bounds, gpui::transparent_black())
                                        .corner_radii(gpui::Corners {
                                            top_left: px(0.0),
                                            top_right: px(0.0),
                                            bottom_right: px(FRAME_RADIUS),
                                            bottom_left: px(FRAME_RADIUS),
                                        })
                                        .border_widths(gpui::Edges {
                                            top: px(0.0),
                                            right: px(FRAME_WIDTH),
                                            bottom: px(FRAME_WIDTH),
                                            left: px(FRAME_WIDTH),
                                        })
                                        .border_color(frame_color),
                                );
                                // The border above has no top edge, so the panel's
                                // rounded top corners would leave see-through notches
                                // below the titlebar. Fill each notch: the corner
                                // square minus the panel's quarter-circle.
                                let radius = px(PANEL_RADIUS);
                                for left_side in [true, false] {
                                    let corner = point(
                                        if left_side {
                                            bounds.left() + px(FRAME_WIDTH)
                                        } else {
                                            bounds.right() - px(FRAME_WIDTH)
                                        },
                                        bounds.top(),
                                    );
                                    let along = if left_side { radius } else { -radius };
                                    let mut path = PathBuilder::fill();
                                    path.move_to(corner);
                                    path.line_to(point(corner.x + along, corner.y));
                                    path.arc_to(
                                        point(radius, radius),
                                        px(0.0),
                                        false,
                                        !left_side,
                                        point(corner.x, corner.y + radius),
                                    );
                                    path.close();
                                    if let Ok(path) = path.build() {
                                        window.paint_path(path, frame_color);
                                    }
                                }
                            },
                        )
                        .absolute()
                        .left_0()
                        .top_0()
                        .right(px(0.0))
                        .bottom(px(0.0)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(FRAME_WIDTH))
                            .top_0()
                            .right(px(FRAME_WIDTH))
                            .bottom(px(FRAME_WIDTH))
                            .overflow_hidden()
                            .rounded(px(PANEL_RADIUS))
                            .border_1()
                            .border_color(rgb(theme.border))
                            .when_some(content, |shell, content| match content {
                                TabContent::Launcher => shell
                                    .bg(rgb(theme.panel))
                                    .child(self.render_launcher(theme, cx)),
                                TabContent::Settings => shell
                                    .bg(rgb(theme.panel))
                                    .child(self.render_settings(theme, cx)),
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
    data_file("recent-workspaces")
}

fn editor_transparency_file() -> Option<PathBuf> {
    data_file("editor-transparency")
}

fn data_file(name: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/Nuvi")
            .join(name)
    })
}

fn load_editor_transparency() -> f32 {
    editor_transparency_file()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(DEFAULT_EDITOR_TRANSPARENCY)
}

fn write_editor_transparency(value: f32) -> std::io::Result<()> {
    let Some(file) = editor_transparency_file() else {
        return Ok(());
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, format!("{value:.2}"))
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
