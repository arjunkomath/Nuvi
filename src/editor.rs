use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::OsString,
    ops::Range,
    time::{Duration, Instant},
};

use async_channel::Receiver;
use gpui::{
    App, AsyncApp, AsyncWindowContext, Bounds, ContentMask, Context, Corners, Entity, EventEmitter,
    FocusHandle, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Hsla, InputHandler,
    KeyDownEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Path, PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, ShapedLine,
    SharedString, Size, StrikethroughStyle, Subscription, TextRun, Timer, UTF16Selection,
    UnderlineStyle, WeakEntity, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use nvim_rs::Value;

use crate::{
    grid::{Cell, CursorShape, RedrawResult, ScrollRecord, Ui},
    nvim::{NvimClient, NvimEvent, NvimSession},
};

// GPUI exposes the proportional system UI font, but not macOS's system monospace font.
const DEFAULT_FONT: &str = "Menlo";
const DEFAULT_FONT_SIZE: f32 = 15.0;
const CURSOR_ANIMATION_LENGTH: f32 = 0.150;
const CURSOR_SHORT_ANIMATION_LENGTH: f32 = 0.040;
const SCROLL_ANIMATION_LENGTH: f32 = 0.300;
const MAX_SCROLL_ROWS: usize = 8;
const EDITOR_CORNER_RADIUS: f32 = 8.0;
const CURSOR_CORNERS: [(f32, f32); 4] = [(-0.5, -0.5), (-0.5, 0.5), (0.5, 0.5), (0.5, -0.5)];

pub struct Editor {
    ui: Ui,
    background_opacity: f32,
    session: Option<NvimSession>,
    status: Option<Status>,
    focus: FocusHandle,
    bounds: Option<Bounds<Pixels>>,
    cell_size: Size<Pixels>,
    requested_size: (usize, usize),
    shape_cache: HashMap<TextKey, ShapedLine>,
    resolved_cells: Vec<ResolvedHighlight>,
    font_spec: Option<String>,
    font_families: Vec<String>,
    font_size: f32,
    font_width: f32,
    font_weight: f32,
    font_italic: bool,
    metrics_linespace: i64,
    marked_text: String,
    marked_selection: Range<usize>,
    scroll_remainder: Point<Pixels>,
    cursor_visible: bool,
    blink_epoch: usize,
    cursor_animation: CursorAnimation,
    scroll_animation: Option<ScrollAnimation>,
    animation_frame: Option<Instant>,
    pending_redraw: Vec<Value>,
    close_requested: bool,
    allow_close: bool,
    subscriptions: Vec<Subscription>,
}

pub enum EditorEvent {
    CloseCancelled,
    Closed,
    Title(SharedString),
}

/// Overlay shown in the corner of the editor. Startup progress clears itself
/// on the first redraw; errors stay until the user dismisses them.
#[derive(Clone)]
enum Status {
    Starting,
    Error(SharedString),
}

// The font and size are deliberately not part of the key: whenever they change,
// `prepare` clears the whole cache.
#[derive(Clone, Hash, PartialEq, Eq)]
struct TextKey {
    text: String,
    foreground: u32,
    special: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strikethrough: bool,
    dim: bool,
}

#[derive(Debug, PartialEq)]
struct ParsedFont {
    families: Vec<String>,
    size: f32,
    width: f32,
    weight: f32,
    italic: bool,
}

#[derive(Default)]
struct PaintLayer {
    backgrounds: Vec<PaintQuad>,
    paths: Vec<(Path<Pixels>, Hsla)>,
    glyphs: Vec<(Point<Pixels>, ShapedLine)>,
}

#[derive(Default)]
struct PaintState {
    main: PaintLayer,
    scroll: Option<MaskedPaint>,
    cursor: Option<MaskedPath>,
}

struct MaskedPaint {
    mask: ContentMask<Pixels>,
    layer: PaintLayer,
}

struct MaskedPath {
    path: Path<Pixels>,
    color: Hsla,
    mask: Option<ContentMask<Pixels>>,
}

#[derive(Clone, Copy, Default)]
struct Spring {
    position: f32,
    velocity: f32,
}

#[derive(Clone, Copy, Default)]
struct CursorCorner {
    current: (f32, f32),
    destination: (f32, f32),
    spring_row: Spring,
    spring_col: Spring,
    animation_length: f32,
}

#[derive(Default)]
struct CursorAnimation {
    corners: [CursorCorner; 4],
    center: (f32, f32),
    initialized: bool,
    animating: bool,
}

struct ScrollAnimation {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
    direction: i8,
    spring: Spring,
    trailing: Vec<Vec<Cell>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PowerlineSeparator {
    HardRight,
    SoftRight,
    HardLeft,
    SoftLeft,
    AngledUpper,
    AngledThin,
    AngledLower,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ResolvedHighlight {
    foreground: u32,
    background: Option<u32>,
    special: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strikethrough: bool,
    dim: bool,
    blend: u8,
}

struct GridPainter<'a> {
    window: &'a mut Window,
    shape_cache: &'a mut HashMap<TextKey, ShapedLine>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    font_families: &'a [String],
    font_size: Pixels,
    font_weight: f32,
    font_italic: bool,
    forced_width: Option<Pixels>,
}

impl GridPainter<'_> {
    fn paint_cells(
        &mut self,
        cells: &[Cell],
        highlights: &[ResolvedHighlight],
        start_col: usize,
        y: Pixels,
        layer: &mut PaintLayer,
    ) {
        debug_assert_eq!(cells.len(), highlights.len());

        let mut run_start = 0;
        while run_start < cells.len() {
            let background = (
                highlights[run_start].background,
                highlights[run_start].blend,
            );
            let mut run_end = run_start + 1;
            while run_end < cells.len()
                && (highlights[run_end].background, highlights[run_end].blend) == background
            {
                run_end += 1;
            }
            let quad_bounds = Bounds::new(
                point(
                    self.bounds.left() + self.cell_width * (start_col + run_start) as f32,
                    y,
                ),
                size(
                    self.cell_width * (run_end - run_start) as f32,
                    self.line_height,
                ),
            );
            let mut corners = Corners::all(px(0.0));
            let touches_left = quad_bounds.left() <= self.bounds.left();
            let touches_right = quad_bounds.right() >= self.bounds.right();
            let touches_top = quad_bounds.top() <= self.bounds.top();
            let touches_bottom = quad_bounds.bottom() >= self.bounds.bottom();
            let radius = px(EDITOR_CORNER_RADIUS);
            if touches_top && touches_left {
                corners.top_left = radius;
            }
            if touches_top && touches_right {
                corners.top_right = radius;
            }
            if touches_bottom && touches_right {
                corners.bottom_right = radius;
            }
            if touches_bottom && touches_left {
                corners.bottom_left = radius;
            }
            if let Some(background_color) = background.0 {
                layer.backgrounds.push(
                    fill(
                        quad_bounds,
                        color(background_color).opacity(1.0 - background.1 as f32 / 100.0),
                    )
                    .corner_radii(corners),
                )
            }
            run_start = run_end;
        }

        let mut col = 0;
        while col < cells.len() {
            if let Some(separator) = powerline_separator(&cells[col].text) {
                let bounds = Bounds::new(
                    point(
                        self.bounds.left() + self.cell_width * (start_col + col) as f32,
                        y,
                    ),
                    size(self.cell_width, self.line_height),
                );
                if let Some(path) = powerline_path(separator, bounds) {
                    let mut foreground = color(highlights[col].foreground);
                    if highlights[col].dim {
                        foreground = foreground.opacity(0.5);
                    }
                    layer.paths.push((path, foreground));
                }
                col += 1;
                continue;
            }

            // A double-width cell is followed by an empty continuation cell. Shape
            // wide cells alone at their own column so a glyph advance that differs
            // from two cell widths cannot push the rest of the run out of grid
            // alignment (forced_width would even collapse it to one column).
            let is_wide = |index: usize| {
                cells
                    .get(index + 1)
                    .is_some_and(|cell| cell.text.is_empty())
            };
            if cells[col].text.is_empty() {
                // Continuation cell; its column is covered by the wide glyph.
                col += 1;
                continue;
            }

            let run_start = col;
            let highlight = highlights[col];
            let mut text = String::new();
            if is_wide(col) {
                text.push_str(&cells[col].text);
                col += 2;
            } else {
                while col < cells.len()
                    && highlights[col] == highlight
                    && powerline_separator(&cells[col].text).is_none()
                    && !cells[col].text.is_empty()
                    && !is_wide(col)
                {
                    text.push_str(&cells[col].text);
                    col += 1;
                }
            }
            if text.is_empty()
                || (!highlight.underline
                    && !highlight.undercurl
                    && !highlight.strikethrough
                    && text.chars().all(char::is_whitespace))
            {
                continue;
            }
            let key = TextKey {
                text,
                foreground: highlight.foreground,
                special: highlight.special,
                bold: highlight.bold,
                italic: highlight.italic,
                underline: highlight.underline,
                undercurl: highlight.undercurl,
                strikethrough: highlight.strikethrough,
                dim: highlight.dim,
            };
            let line = match self.shape_cache.entry(key) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let weight = if highlight.bold {
                        FontWeight(self.font_weight.max(FontWeight::BOLD.0))
                    } else {
                        FontWeight(self.font_weight)
                    };
                    let font = grid_font(
                        self.font_families,
                        weight,
                        self.font_italic || highlight.italic,
                    );
                    let mut foreground = color(highlight.foreground);
                    if highlight.dim {
                        foreground = foreground.opacity(0.5);
                    }
                    let line = shape(
                        self.window,
                        &entry.key().text,
                        self.font_size,
                        &font,
                        foreground,
                        (highlight.underline || highlight.undercurl).then_some(UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(color(highlight.special)),
                            wavy: highlight.undercurl,
                        }),
                        highlight.strikethrough.then_some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: Some(foreground),
                        }),
                        self.forced_width,
                    );
                    entry.insert(line)
                }
            };
            layer.glyphs.push((
                point(
                    self.bounds.left() + self.cell_width * (start_col + run_start) as f32,
                    y,
                ),
                line.clone(),
            ));
        }
    }
}

impl Editor {
    pub fn new(
        window: &Window,
        args: Vec<OsString>,
        working_directory: Option<std::path::PathBuf>,
        background_opacity: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, receiver) = async_channel::bounded(256);
        cx.spawn(async move |editor: WeakEntity<Editor>, cx: &mut AsyncApp| {
            match NvimSession::spawn(args, working_directory, sender).await {
                Ok(session) => {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.session = Some(session);
                        editor.clear_starting_status();
                        if editor.close_requested {
                            editor.session.as_ref().unwrap().client.confirm_quit();
                        }
                        cx.notify();
                    });
                }
                Err(error) => {
                    let _ = editor.update(cx, |editor, cx| {
                        editor.status = Some(Status::Error(error.into()));
                        if editor.close_requested {
                            editor.allow_close = true;
                            cx.emit(EditorEvent::Closed);
                        } else {
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();

        Self::listen(window, receiver, cx);

        Self {
            ui: Ui::default(),
            background_opacity,
            session: None,
            status: Some(Status::Starting),
            focus: cx.focus_handle(),
            bounds: None,
            cell_size: size(px(8.0), px(20.0)),
            requested_size: (0, 0),
            shape_cache: HashMap::new(),
            resolved_cells: Vec::new(),
            font_spec: None,
            font_families: vec![DEFAULT_FONT.into()],
            font_size: DEFAULT_FONT_SIZE,
            font_width: 0.0,
            font_weight: FontWeight::NORMAL.0,
            font_italic: false,
            metrics_linespace: i64::MIN,
            marked_text: String::new(),
            marked_selection: 0..0,
            scroll_remainder: point(px(0.0), px(0.0)),
            cursor_visible: true,
            blink_epoch: 0,
            cursor_animation: CursorAnimation::default(),
            scroll_animation: None,
            animation_frame: None,
            pending_redraw: Vec::new(),
            close_requested: false,
            allow_close: false,
            subscriptions: Vec::new(),
        }
    }

    pub fn bind_window(editor: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let focus = editor.read(cx).focus.clone();
        focus.focus(window);

        let weak = editor.downgrade();
        let focused = window.on_focus_in(&focus, cx, move |_, cx| {
            let _ = weak.update(cx, |editor, _| editor.set_focus(true));
        });
        let weak = editor.downgrade();
        let blurred = window.on_focus_out(&focus, cx, move |_, _, cx| {
            let _ = weak.update(cx, |editor, _| editor.set_focus(false));
        });
        editor.update(cx, |editor, _| {
            editor.subscriptions.extend([focused, blurred]);
        });
    }

    pub fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.background_opacity = opacity.clamp(0.0, 1.0);
        cx.notify();
    }

    fn listen(window: &Window, receiver: Receiver<NvimEvent>, cx: &Context<Self>) {
        cx.spawn_in(
            window,
            async move |editor: WeakEntity<Editor>, cx: &mut AsyncWindowContext| {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        NvimEvent::Redraw(args) => {
                            let update = editor.update_in(cx, move |editor, _, cx| {
                                // Earlier batches were already scanned, so only the new
                                // events can carry the flush.
                                let flushed = redraw_has_flush(&args);
                                editor.pending_redraw.extend(args);
                                if !flushed {
                                    return;
                                }

                                let before = (
                                    editor.ui.grid.cursor_row,
                                    editor.ui.grid.cursor_col,
                                    editor.ui.mode_index,
                                    editor.ui.busy,
                                );
                                let redraw = editor
                                    .ui
                                    .apply_redraw(&std::mem::take(&mut editor.pending_redraw));
                                let restart_blink = redraw.invalidated
                                    || before
                                        != (
                                            editor.ui.grid.cursor_row,
                                            editor.ui.grid.cursor_col,
                                            editor.ui.mode_index,
                                            editor.ui.busy,
                                        );
                                let title_changed = redraw.title_changed;
                                editor.apply_animations(redraw, Instant::now());
                                if editor.shape_cache.len() > 16_384 {
                                    editor.shape_cache.clear();
                                }
                                if restart_blink {
                                    editor.restart_blink(cx);
                                }
                                if title_changed {
                                    cx.emit(EditorEvent::Title(editor.ui.title.clone().into()));
                                }
                                editor.clear_starting_status();
                                cx.notify();
                            });
                            if update.is_err() {
                                break;
                            }
                        }
                        NvimEvent::Error(error) => {
                            let update = editor.update_in(cx, |editor, _, cx| {
                                editor.status = Some(Status::Error(error.into()));
                                cx.notify();
                            });
                            if update.is_err() {
                                break;
                            }
                        }
                        NvimEvent::CloseCancelled => {
                            let update = editor.update_in(cx, |editor, _, cx| {
                                editor.close_requested = false;
                                cx.emit(EditorEvent::CloseCancelled);
                            });
                            if update.is_err() {
                                break;
                            }
                        }
                        NvimEvent::Exited(error) => {
                            let _ = editor.update_in(cx, |editor, _, cx| {
                                editor.session = None;
                                editor.allow_close = true;
                                if editor.close_requested || error.is_none() {
                                    cx.emit(EditorEvent::Closed);
                                } else {
                                    editor.status = Some(Status::Error(error.unwrap().into()));
                                    cx.notify();
                                }
                            });
                            break;
                        }
                    }
                }
            },
        )
        .detach();
    }

    /// Shows the cursor and, when the current mode blinks, schedules the next
    /// toggle. Bumping the epoch cancels any tick already in flight, so the
    /// blink chain parks itself whenever blinking is disabled and costs nothing
    /// until the next call re-arms it.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_epoch += 1;
        self.cursor_visible = true;
        let mode = self.ui.cursor_mode();
        let (wait, on) = (mode.blink_wait, mode.blink_on);
        if self.blinking() {
            // Neovim's blinkwait is the delay before blinking starts after activity.
            self.schedule_blink(if wait > 0 { wait } else { on }, cx);
        }
    }

    fn blinking(&self) -> bool {
        let mode = self.ui.cursor_mode();
        mode.blink_on > 0 && mode.blink_off > 0 && !self.ui.busy && !self.animating()
    }

    fn schedule_blink(&self, delay: u64, cx: &mut Context<Self>) {
        let epoch = self.blink_epoch;
        cx.spawn(async move |editor: WeakEntity<Editor>, cx: &mut AsyncApp| {
            Timer::after(Duration::from_millis(delay.max(20))).await;
            let _ = editor.update(cx, |editor, cx| editor.blink_tick(epoch, cx));
        })
        .detach();
    }

    fn blink_tick(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch {
            return;
        }
        if !self.blinking() {
            self.cursor_visible = true;
            return;
        }
        self.cursor_visible = !self.cursor_visible;
        let mode = self.ui.cursor_mode();
        let delay = if self.cursor_visible {
            mode.blink_on
        } else {
            mode.blink_off
        };
        self.schedule_blink(delay, cx);
        cx.notify();
    }

    fn apply_animations(&mut self, redraw: RedrawResult, now: Instant) {
        if redraw.invalidated || self.ui.busy || reduce_motion() {
            self.snap_animations();
            return;
        }

        let was_animating = self.animating();
        let mut snapped_scroll = false;
        for scroll in redraw.scrolls {
            snapped_scroll |= !self.absorb_scroll(scroll);
        }

        let mode = self.ui.cursor_mode().clone();
        let target = self.cursor_center();
        if snapped_scroll {
            self.cursor_animation
                .snap(target, mode.shape, mode.cell_percentage);
        } else {
            self.cursor_animation
                .retarget(target, mode.shape, mode.cell_percentage);
        }

        if !was_animating && self.animating() {
            self.animation_frame = Some(now);
        }
    }

    fn absorb_scroll(&mut self, scroll: ScrollRecord) -> bool {
        let distance = scroll.rows.unsigned_abs() as usize;
        if distance == 0
            || distance > MAX_SCROLL_ROWS
            || distance >= scroll.bottom.saturating_sub(scroll.top)
        {
            self.scroll_animation = None;
            return false;
        }

        let direction = scroll.rows.signum() as i8;
        if let Some(animation) = &mut self.scroll_animation {
            if animation.compatible(&scroll) {
                animation.absorb(scroll);
                return true;
            }
            self.scroll_animation = None;
            return false;
        }

        self.scroll_animation = Some(ScrollAnimation {
            top: scroll.top,
            bottom: scroll.bottom,
            left: scroll.left,
            right: scroll.right,
            direction,
            spring: Spring {
                position: scroll.rows as f32,
                velocity: 0.0,
            },
            trailing: scroll.evicted,
        });
        true
    }

    fn cursor_center(&self) -> (f32, f32) {
        (
            self.ui.grid.cursor_row as f32
                + 0.5
                + self.scroll_offset_for(self.ui.grid.cursor_row, self.ui.grid.cursor_col),
            self.ui.grid.cursor_col as f32 + 0.5,
        )
    }

    fn scroll_offset_for(&self, row: usize, col: usize) -> f32 {
        self.scroll_animation
            .as_ref()
            .filter(|animation| animation.contains(row, col))
            .map(ScrollAnimation::offset)
            .unwrap_or(0.0)
    }

    fn animating(&self) -> bool {
        self.cursor_animation.animating || self.scroll_animation.is_some()
    }

    fn snap_animations(&mut self) {
        self.scroll_animation = None;
        let mode = self.ui.cursor_mode().clone();
        self.cursor_animation
            .snap(self.cursor_center(), mode.shape, mode.cell_percentage);
        self.animation_frame = None;
    }

    fn advance_animations(&mut self, now: Instant) {
        if !self.animating() {
            self.animation_frame = None;
            return;
        }

        let dt = self
            .animation_frame
            .replace(now)
            .map(|previous| now.duration_since(previous).as_secs_f32())
            .unwrap_or(0.0);
        let scroll_before = self
            .scroll_animation
            .as_ref()
            .map(ScrollAnimation::offset)
            .unwrap_or(0.0);
        let scrolling = self
            .scroll_animation
            .as_mut()
            .is_some_and(|animation| animation.spring.update(dt, SCROLL_ANIMATION_LENGTH));
        let scroll_after = self
            .scroll_animation
            .as_ref()
            .map(ScrollAnimation::offset)
            .unwrap_or(0.0);

        if self.scroll_animation.as_ref().is_some_and(|animation| {
            animation.contains(self.ui.grid.cursor_row, self.ui.grid.cursor_col)
        }) {
            // cursor_center() already includes the live scroll offset, so shifting
            // the cursor animation by this frame's scroll delta makes the retarget
            // below a no-op. Removing either side of that cancellation makes the
            // cursor track the scroll twice.
            self.cursor_animation
                .shift(scroll_after - scroll_before, 0.0);
        }
        if !scrolling {
            self.scroll_animation = None;
        }

        let mode = self.ui.cursor_mode().clone();
        self.cursor_animation
            .retarget(self.cursor_center(), mode.shape, mode.cell_percentage);
        self.cursor_animation.advance(dt);

        if !self.animating() {
            self.animation_frame = None;
        }
    }

    fn clear_starting_status(&mut self) {
        if matches!(self.status, Some(Status::Starting)) {
            self.status = None;
        }
    }

    fn set_focus(&mut self, gained: bool) {
        if let Some(session) = &self.session {
            session.client.focus(gained);
        }
    }

    pub fn activate(&mut self, window: &mut Window) {
        self.focus.focus(window);
        self.set_focus(true);
    }

    pub fn deactivate(&mut self) {
        self.set_focus(false);
    }

    pub(crate) fn request_close(&mut self) -> bool {
        if self.allow_close
            || self.session.is_none() && matches!(self.status, Some(Status::Error(_)))
        {
            return true;
        }
        if self.close_requested {
            return false;
        }
        self.close_requested = true;
        if let Some(session) = &self.session {
            session.client.confirm_quit();
        }
        false
    }

    fn client(&self) -> Option<&NvimClient> {
        self.session.as_ref().map(|session| &session.client)
    }

    fn send_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !text.is_empty()
            && let Some(client) = self.client()
        {
            client.input(text.replace('<', "<LT>"));
        }
        self.restart_blink(cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(keys) = encode_key(&event.keystroke) {
            if let Some(client) = self.client() {
                client.input(keys);
            }
            self.restart_blink(cx);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window);
        self.mouse(event.button, "press", event.position, event.modifiers);
        cx.stop_propagation();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.mouse(event.button, "release", event.position, event.modifiers);
        cx.stop_propagation();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let (button, action) = match event.pressed_button {
            Some(button) => (mouse_button(button), "drag"),
            None => ("move", ""),
        };
        self.mouse_named(button, action, event.position, event.modifiers);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => delta,
            ScrollDelta::Lines(delta) => point(
                self.cell_size.width * delta.x,
                self.cell_size.height * delta.y,
            ),
        };
        self.scroll_remainder.x += delta.x;
        self.scroll_remainder.y += delta.y;
        let Some((row, col)) = self.point_to_cell(event.position) else {
            return;
        };
        let modifiers = modifiers(event.modifiers);
        let Some(client) = self.client().cloned() else {
            return;
        };

        while self.scroll_remainder.y.abs() >= self.cell_size.height {
            let action = if self.scroll_remainder.y > px(0.0) {
                self.scroll_remainder.y -= self.cell_size.height;
                "up"
            } else {
                self.scroll_remainder.y += self.cell_size.height;
                "down"
            };
            client.mouse("wheel", action, modifiers.clone(), row, col);
        }
        while self.scroll_remainder.x.abs() >= self.cell_size.width {
            let action = if self.scroll_remainder.x > px(0.0) {
                self.scroll_remainder.x -= self.cell_size.width;
                "left"
            } else {
                self.scroll_remainder.x += self.cell_size.width;
                "right"
            };
            client.mouse("wheel", action, modifiers.clone(), row, col);
        }
        cx.stop_propagation();
    }

    fn mouse(
        &self,
        button: MouseButton,
        action: &'static str,
        position: Point<Pixels>,
        held: Modifiers,
    ) {
        self.mouse_named(mouse_button(button), action, position, held);
    }

    fn mouse_named(
        &self,
        button: &'static str,
        action: &'static str,
        position: Point<Pixels>,
        held: Modifiers,
    ) {
        if !self.ui.mouse_enabled {
            return;
        }
        if let (Some(client), Some((row, col))) = (self.client(), self.point_to_cell(position)) {
            client.mouse(button, action, modifiers(held), row, col);
        }
    }

    fn point_to_cell(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        if self.ui.grid.width == 0 || self.ui.grid.height == 0 {
            return None;
        }
        let bounds = self.bounds?;
        let row = ((position.y - bounds.top()) / self.cell_size.height).floor() as usize;
        let col = ((position.x - bounds.left()) / self.cell_size.width).floor() as usize;
        Some((
            row.min(self.ui.grid.height.saturating_sub(1)),
            col.min(self.ui.grid.width.saturating_sub(1)),
        ))
    }

    fn prepare(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaintState {
        let now = Instant::now();
        if self.bounds.is_some_and(|previous| previous != bounds) {
            self.snap_animations();
        }
        self.bounds = Some(bounds);
        let font_changed = self.font_spec.as_deref() != Some(self.ui.guifont.as_str());
        if font_changed {
            let parsed = parse_guifont(&self.ui.guifont);
            self.font_families =
                installed_font_families(parsed.families, &window.text_system().all_font_names());
            self.font_size = parsed.size;
            self.font_width = parsed.width;
            self.font_weight = parsed.weight;
            self.font_italic = parsed.italic;
            self.font_spec = Some(self.ui.guifont.clone());
            self.shape_cache.clear();
        }
        let font_size = self.font_size;
        let font_weight = self.font_weight;
        let font_italic = self.font_italic;
        let font_size = px(font_size);
        if font_changed || self.metrics_linespace != self.ui.linespace {
            let base_font = grid_font(&self.font_families, FontWeight(font_weight), font_italic);
            let font_id = window.text_system().resolve_font(&base_font);
            let cell_width = (window
                .text_system()
                .ch_advance(font_id, font_size)
                .unwrap_or(font_size * 0.6)
                + px(self.font_width))
            .max(px(1.0));
            let line_height = grid_line_height(
                window.text_system().ascent(font_id, font_size),
                window.text_system().descent(font_id, font_size),
                self.ui.linespace,
            );
            self.cell_size = size(cell_width, line_height);
            self.metrics_linespace = self.ui.linespace;
        }
        let cell_width = self.cell_size.width;
        let line_height = self.cell_size.height;
        let forced_width = (self.font_width != 0.0).then_some(cell_width);

        let requested = (
            ((bounds.size.width / cell_width).floor() as usize).max(1),
            ((bounds.size.height / line_height).floor() as usize).max(1),
        );
        if requested != self.requested_size {
            self.snap_animations();
            self.requested_size = requested;
            if let Some(client) = self.client() {
                client.resize(requested.0, requested.1);
            }
        }

        let was_animating = self.animating();
        self.advance_animations(now);
        if was_animating && !self.animating() {
            // Blinking pauses during animations; re-arm it once they settle.
            self.restart_blink(cx);
        }

        let mut paint = PaintState::default();
        paint.main.backgrounds.push(
            fill(
                bounds,
                color(self.ui.default_background).opacity(self.background_opacity),
            )
            .corner_radii(px(EDITOR_CORNER_RADIUS)),
        );
        let visible_rows = self.ui.grid.height.min(requested.1);
        let visible_cols = self.ui.grid.width.min(requested.0);
        let mut resolved = std::mem::take(&mut self.resolved_cells);
        resolved.clear();
        resolved.extend(
            self.ui
                .grid
                .cells
                .chunks(self.ui.grid.width.max(1))
                .take(visible_rows)
                .flat_map(|row| row.iter().take(visible_cols))
                .map(|cell| self.resolved(cell.highlight)),
        );
        let trailing_highlights = self
            .scroll_animation
            .as_ref()
            .map(|animation| {
                animation
                    .trailing
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| self.resolved(cell.highlight))
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut scroll_layer = PaintLayer::default();
        {
            let mut painter = GridPainter {
                window,
                shape_cache: &mut self.shape_cache,
                bounds,
                cell_width,
                line_height,
                font_families: &self.font_families,
                font_size,
                font_weight,
                font_italic,
                forced_width,
            };
            for row in 0..visible_rows {
                let cells = &self.ui.grid.cells
                    [row * self.ui.grid.width..row * self.ui.grid.width + visible_cols];
                let highlights = &resolved[row * visible_cols..(row + 1) * visible_cols];
                if let Some(animation) = self.scroll_animation.as_ref().filter(|animation| {
                    row >= animation.top
                        && row < animation.bottom
                        && animation.left.min(visible_cols) < animation.right.min(visible_cols)
                }) {
                    let left = animation.left.min(visible_cols);
                    let right = animation.right.min(visible_cols);
                    painter.paint_cells(
                        &cells[..left],
                        &highlights[..left],
                        0,
                        bounds.top() + line_height * row as f32,
                        &mut paint.main,
                    );
                    painter.paint_cells(
                        &cells[left..right],
                        &highlights[left..right],
                        left,
                        bounds.top() + line_height * (row as f32 + animation.offset()),
                        &mut scroll_layer,
                    );
                    painter.paint_cells(
                        &cells[right..],
                        &highlights[right..],
                        right,
                        bounds.top() + line_height * row as f32,
                        &mut paint.main,
                    );
                } else {
                    painter.paint_cells(
                        cells,
                        highlights,
                        0,
                        bounds.top() + line_height * row as f32,
                        &mut paint.main,
                    );
                }
            }

            if let Some(animation) = &self.scroll_animation {
                let left = animation.left.min(visible_cols);
                let right = animation.right.min(visible_cols);
                if left < right {
                    let slice_start = left - animation.left;
                    let slice_end = slice_start + right - left;
                    let offset = animation.offset();
                    for (index, (cells, highlights)) in animation
                        .trailing
                        .iter()
                        .zip(&trailing_highlights)
                        .enumerate()
                    {
                        let row = if animation.direction > 0 {
                            animation.top as f32 + index as f32 - animation.trailing.len() as f32
                                + offset
                        } else {
                            animation.bottom as f32 + index as f32 + offset
                        };
                        painter.paint_cells(
                            &cells[slice_start..slice_end],
                            &highlights[slice_start..slice_end],
                            left,
                            bounds.top() + line_height * row,
                            &mut scroll_layer,
                        );
                    }
                }
            }
        }
        self.resolved_cells = resolved;

        let scroll_mask = self.scroll_animation.as_ref().and_then(|animation| {
            let top = animation.top.min(visible_rows);
            let bottom = animation.bottom.min(visible_rows);
            let left = animation.left.min(visible_cols);
            let right = animation.right.min(visible_cols);
            (top < bottom && left < right).then(|| ContentMask {
                bounds: Bounds::new(
                    point(
                        bounds.left() + cell_width * left as f32,
                        bounds.top() + line_height * top as f32,
                    ),
                    size(
                        cell_width * (right - left) as f32,
                        line_height * (bottom - top) as f32,
                    ),
                ),
            })
        });
        if let Some(mask) = scroll_mask.clone() {
            paint.scroll = Some(MaskedPaint {
                mask,
                layer: scroll_layer,
            });
        }

        if !self.marked_text.is_empty() {
            let font = grid_font(&self.font_families, FontWeight(font_weight), font_italic);
            let line = shape(
                window,
                &self.marked_text,
                font_size,
                &font,
                color(self.ui.default_foreground),
                Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color(0x5e81ac)),
                    wavy: false,
                }),
                None,
                forced_width,
            );
            paint.main.glyphs.push((
                point(
                    bounds.left() + cell_width * self.ui.grid.cursor_col as f32,
                    bounds.top() + line_height * self.ui.grid.cursor_row as f32,
                ),
                line,
            ));
        }

        if self.cursor_visible
            && !self.ui.busy
            && self.ui.grid.cursor_row < visible_rows
            && self.ui.grid.cursor_col < visible_cols
        {
            let mode = self.ui.cursor_mode();
            let cursor_color = if mode.attr_id == 0 {
                self.ui.default_foreground
            } else {
                self.resolved(mode.attr_id)
                    .background
                    .unwrap_or(self.ui.default_background)
            };
            let mut builder = PathBuilder::fill();
            for (index, (row, col)) in self.cursor_animation.points().into_iter().enumerate() {
                let point = point(
                    bounds.left() + cell_width * col,
                    bounds.top() + line_height * row,
                );
                if index == 0 {
                    builder.move_to(point);
                } else {
                    builder.line_to(point);
                }
            }
            builder.close();
            if let Ok(path) = builder.build() {
                paint.cursor = Some(MaskedPath {
                    path,
                    color: color(cursor_color).opacity(0.65),
                    mask: self
                        .scroll_animation
                        .as_ref()
                        .filter(|animation| {
                            animation.contains(self.ui.grid.cursor_row, self.ui.grid.cursor_col)
                        })
                        .and(scroll_mask),
                });
            }
        }

        if self.animating() {
            window.request_animation_frame();
        }

        paint
    }

    fn resolved(&self, id: u64) -> ResolvedHighlight {
        let highlight = self.ui.highlights.get(&id).cloned().unwrap_or_default();
        let mut foreground = highlight.foreground.unwrap_or(self.ui.default_foreground);
        let mut background = highlight.background;
        if highlight.reverse {
            let reverse_background = foreground;
            foreground = background.unwrap_or(self.ui.default_background);
            background = Some(reverse_background);
        }
        ResolvedHighlight {
            foreground,
            background,
            special: highlight.special.unwrap_or(self.ui.default_special),
            bold: highlight.bold,
            italic: highlight.italic,
            underline: highlight.underline
                || highlight.underdouble
                || highlight.underdotted
                || highlight.underdashed,
            undercurl: highlight.undercurl,
            strikethrough: highlight.strikethrough,
            dim: highlight.dim,
            blend: highlight.blend,
        }
    }

    fn commit_marked_text(&mut self, cx: &mut Context<Self>) {
        if !self.marked_text.is_empty() {
            let text = std::mem::take(&mut self.marked_text);
            self.marked_selection = 0..0;
            self.send_text(&text, cx);
        }
    }
}

impl Spring {
    fn update(&mut self, dt: f32, animation_length: f32) -> bool {
        if animation_length <= dt {
            self.reset();
            return false;
        }
        if self.position == 0.0 {
            return false;
        }

        // Analytic critically damped spring. Omega reaches within 2% of the target at
        // animation_length, and the retained velocity keeps rapid retargeting continuous.
        let omega = 4.0 / animation_length;
        let a = self.position;
        let b = self.position * omega + self.velocity;
        let decay = (-omega * dt).exp();
        self.position = (a + b * dt) * decay;
        self.velocity = decay * (-a * omega - b * dt * omega + b);

        if self.position.abs() < 0.01 {
            self.reset();
            false
        } else {
            true
        }
    }

    fn reset(&mut self) {
        self.position = 0.0;
        self.velocity = 0.0;
    }
}

impl CursorAnimation {
    fn snap(&mut self, center: (f32, f32), shape: CursorShape, cell_percentage: u8) {
        let relative = cursor_corners(shape, cell_percentage);
        for (corner, relative) in self.corners.iter_mut().zip(relative) {
            let destination = (center.0 + relative.0, center.1 + relative.1);
            corner.current = destination;
            corner.destination = destination;
            corner.spring_row.reset();
            corner.spring_col.reset();
            corner.animation_length = 0.0;
        }
        self.center = center;
        self.initialized = true;
        self.animating = false;
    }

    /// Cursor-motion smear: corners whose outward direction aligns with the
    /// travel direction keep the full animation length (they trail), while
    /// corners facing the destination snap almost immediately (they lead), so
    /// the block stretches toward the target and catches up from behind.
    fn retarget(&mut self, center: (f32, f32), shape: CursorShape, cell_percentage: u8) {
        // Movements below this threshold count as staying on the same row/column.
        const EPSILON: f32 = 0.001;
        // Same-row hops of up to two columns are ordinary typing (one column, or
        // two after a wide character); they use the short uniform animation
        // instead of the smear.
        const TYPING_COLUMNS: f32 = 2.0 + EPSILON;

        if !self.initialized {
            self.snap(center, shape, cell_percentage);
            return;
        }

        let relative = cursor_corners(shape, cell_percentage);
        let destinations = relative.map(|relative| (center.0 + relative.0, center.1 + relative.1));
        let movement = (center.0 - self.center.0, center.1 - self.center.1);
        let moved = movement.0.abs() > EPSILON || movement.1.abs() > EPSILON;
        let short_horizontal = movement.0.abs() <= EPSILON && movement.1.abs() <= TYPING_COLUMNS;

        let lengths = if moved && !short_horizontal {
            let mut alignments = [0.0; 4];
            for index in 0..4 {
                let travel = (
                    destinations[index].0 - self.corners[index].destination.0,
                    destinations[index].1 - self.corners[index].destination.1,
                );
                alignments[index] = dot_normalized(travel, relative[index]);
            }
            let min = alignments.iter().copied().fold(f32::INFINITY, f32::min);
            let max = alignments.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let range = max - min;
            alignments.map(|alignment| {
                let alignment = if range > f32::EPSILON {
                    ((alignment - min) / range).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                CURSOR_ANIMATION_LENGTH * (1.0 - alignment)
            })
        } else {
            [CURSOR_SHORT_ANIMATION_LENGTH; 4]
        };

        for index in 0..4 {
            let corner = &mut self.corners[index];
            let destination = destinations[index];
            if destination != corner.destination {
                corner.animation_length = lengths[index];
                corner.spring_row.position = destination.0 - corner.current.0;
                corner.spring_col.position = destination.1 - corner.current.1;
                corner.destination = destination;
                self.animating = true;
            }
        }
        self.center = center;
    }

    fn advance(&mut self, dt: f32) {
        if !self.animating {
            return;
        }

        let mut animating = false;
        for corner in &mut self.corners {
            animating |= corner.spring_row.update(dt, corner.animation_length);
            animating |= corner.spring_col.update(dt, corner.animation_length);
            corner.current = (
                corner.destination.0 - corner.spring_row.position,
                corner.destination.1 - corner.spring_col.position,
            );
        }
        self.animating = animating;
    }

    fn shift(&mut self, rows: f32, cols: f32) {
        if !self.initialized || (rows == 0.0 && cols == 0.0) {
            return;
        }
        self.center.0 += rows;
        self.center.1 += cols;
        for corner in &mut self.corners {
            corner.current.0 += rows;
            corner.current.1 += cols;
            corner.destination.0 += rows;
            corner.destination.1 += cols;
        }
    }

    fn points(&self) -> [(f32, f32); 4] {
        self.corners.map(|corner| corner.current)
    }
}

impl ScrollAnimation {
    fn compatible(&self, scroll: &ScrollRecord) -> bool {
        self.top == scroll.top
            && self.bottom == scroll.bottom
            && self.left == scroll.left
            && self.right == scroll.right
            && self.direction == scroll.rows.signum() as i8
            && self.trailing.len() + scroll.evicted.len() <= MAX_SCROLL_ROWS
    }

    fn absorb(&mut self, mut scroll: ScrollRecord) {
        self.spring.position += scroll.rows as f32;
        if self.direction > 0 {
            self.trailing.append(&mut scroll.evicted);
        } else {
            scroll.evicted.append(&mut self.trailing);
            self.trailing = scroll.evicted;
        }
    }

    fn offset(&self) -> f32 {
        self.spring.position
    }

    fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.top && row < self.bottom && col >= self.left && col < self.right
    }
}

fn cursor_corners(shape: CursorShape, cell_percentage: u8) -> [(f32, f32); 4] {
    let percentage = cell_percentage.max(1) as f32 / 100.0;
    CURSOR_CORNERS.map(|(row, col)| match shape {
        CursorShape::Block => (row, col),
        CursorShape::Vertical => (row, (col + 0.5) * percentage - 0.5),
        CursorShape::Horizontal => (-((-row + 0.5) * percentage - 0.5), col),
    })
}

fn dot_normalized(a: (f32, f32), b: (f32, f32)) -> f32 {
    let a_length = a.0.hypot(a.1);
    let b_length = b.0.hypot(b.1);
    if a_length <= f32::EPSILON || b_length <= f32::EPSILON {
        0.0
    } else {
        (a.0 * b.0 + a.1 * b.1) / (a_length * b_length)
    }
}

fn redraw_has_flush(events: &[Value]) -> bool {
    events.iter().any(|event| {
        event
            .as_array()
            .and_then(|event| event.first())
            .and_then(Value::as_str)
            == Some("flush")
    })
}

#[cfg(target_os = "macos")]
fn reduce_motion() -> bool {
    use objc::{
        Message,
        runtime::{BOOL, Class, NO, Object, Sel},
    };

    // SAFETY: both selectors are stable AppKit APIs and return non-owning values.
    unsafe {
        let Some(class) = Class::get("NSWorkspace") else {
            return false;
        };
        let Ok(workspace): Result<*mut Object, _> =
            class.send_message(Sel::register("sharedWorkspace"), ())
        else {
            return false;
        };
        let Some(workspace) = workspace.as_ref() else {
            return false;
        };
        workspace
            .send_message::<_, BOOL>(Sel::register("accessibilityDisplayShouldReduceMotion"), ())
            .is_ok_and(|reduced| reduced != NO)
    }
}

#[cfg(not(target_os = "macos"))]
fn reduce_motion() -> bool {
    false
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        let input = editor.clone();
        let status = self.status.clone();
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .cursor(gpui::CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_any_mouse_down(cx.listener(Self::on_mouse_down))
            .capture_any_mouse_up(cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        editor.update(cx, |editor, cx| editor.prepare(bounds, window, cx))
                    },
                    move |bounds, paint, window, cx| {
                        let (focus, line_height) = {
                            let editor = input.read(cx);
                            (editor.focus.clone(), editor.cell_size.height)
                        };
                        window.handle_input(
                            &focus,
                            NvimInputHandler {
                                editor: input.clone(),
                                bounds,
                            },
                            cx,
                        );
                        for quad in paint.main.backgrounds {
                            window.paint_quad(quad);
                        }
                        for (path, color) in paint.main.paths {
                            window.paint_path(path, color);
                        }
                        for (origin, line) in paint.main.glyphs {
                            let _ = line.paint(origin, line_height, window, cx);
                        }
                        if let Some(scroll) = paint.scroll {
                            window.with_content_mask(Some(scroll.mask), |window| {
                                for quad in scroll.layer.backgrounds {
                                    window.paint_quad(quad);
                                }
                                for (path, color) in scroll.layer.paths {
                                    window.paint_path(path, color);
                                }
                                for (origin, line) in scroll.layer.glyphs {
                                    let _ = line.paint(origin, line_height, window, cx);
                                }
                            });
                        }
                        if let Some(cursor) = paint.cursor {
                            window.with_content_mask(cursor.mask, |window| {
                                window.paint_path(cursor.path, cursor.color);
                            });
                        }
                    },
                )
                .size_full(),
            )
            .when_some(status, |view, status| {
                let (message, dismissible) = match status {
                    Status::Starting => ("Starting Neovim…".into(), false),
                    Status::Error(message) => (message, true),
                };
                view.child(
                    div()
                        .absolute()
                        .left_4()
                        .bottom_4()
                        .max_w(px(720.0))
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(color(0x2e3440).opacity(0.94))
                        .text_color(rgb(0xeceff4))
                        .flex()
                        .items_start()
                        .gap(px(8.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().min_w(px(0.0)).flex_1().child(message))
                        .when(dismissible, |toast| {
                            toast.child(
                                div()
                                    .id("dismiss-status")
                                    .flex_none()
                                    .size(px(20.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.0))
                                    .text_size(px(14.0))
                                    .cursor_pointer()
                                    .hover(|this| this.bg(color(0xffffff).opacity(0.12)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.status = None;
                                        cx.notify();
                                    }))
                                    .child("×"),
                            )
                        }),
                )
            })
    }
}

impl EventEmitter<EditorEvent> for Editor {}

struct NvimInputHandler {
    editor: Entity<Editor>,
    bounds: Bounds<Pixels>,
}

impl InputHandler for NvimInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let selection = self.editor.read(cx).marked_selection.clone();
        Some(UTF16Selection {
            range: selection,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let text = &self.editor.read(cx).marked_text;
        (!text.is_empty()).then(|| 0..text.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        _: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let text = self.editor.read(cx).marked_text.clone();
        *adjusted = Some(0..text.encode_utf16().count());
        Some(text)
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.marked_text.clear();
            editor.marked_selection = 0..0;
            editor.send_text(text, cx);
            cx.notify();
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        new_selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.marked_text = new_text.into();
            editor.marked_selection = new_selection.unwrap_or_else(|| {
                let end = new_text.encode_utf16().count();
                end..end
            });
            cx.notify();
        });
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.editor.update(cx, |editor, cx| {
            editor.commit_marked_text(cx);
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let editor = self.editor.read(cx);
        Some(Bounds::new(
            point(
                self.bounds.left() + editor.cell_size.width * editor.ui.grid.cursor_col as f32,
                self.bounds.top() + editor.cell_size.height * editor.ui.grid.cursor_row as f32,
            ),
            editor.cell_size,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        Some(0)
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "these are the GPUI text shaping inputs"
)]
fn shape(
    window: &mut Window,
    text: &str,
    font_size: Pixels,
    font: &Font,
    color: Hsla,
    underline: Option<UnderlineStyle>,
    strikethrough: Option<StrikethroughStyle>,
    forced_width: Option<Pixels>,
) -> ShapedLine {
    window.text_system().shape_line(
        text.to_string().into(),
        font_size,
        &[TextRun {
            len: text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline,
            strikethrough,
        }],
        forced_width,
    )
}

fn powerline_separator(text: &str) -> Option<PowerlineSeparator> {
    match text {
        "\u{e0b0}" => Some(PowerlineSeparator::HardRight),
        "\u{e0b1}" => Some(PowerlineSeparator::SoftRight),
        "\u{e0b2}" => Some(PowerlineSeparator::HardLeft),
        "\u{e0b3}" => Some(PowerlineSeparator::SoftLeft),
        "\u{e0ba}" => Some(PowerlineSeparator::AngledLower),
        "\u{e0bb}" | "\u{e0bd}" => Some(PowerlineSeparator::AngledThin),
        "\u{e0bc}" => Some(PowerlineSeparator::AngledUpper),
        _ => None,
    }
}

fn powerline_path(separator: PowerlineSeparator, bounds: Bounds<Pixels>) -> Option<Path<Pixels>> {
    let left = bounds.left();
    let right = bounds.right();
    let top = bounds.top();
    let bottom = bounds.bottom();
    let middle = top + bounds.size.height / 2.0;
    let overlap = px(0.5);
    let mut path = if matches!(
        separator,
        PowerlineSeparator::SoftRight
            | PowerlineSeparator::SoftLeft
            | PowerlineSeparator::AngledThin
    ) {
        PathBuilder::stroke(px(1.0))
    } else {
        PathBuilder::fill()
    };

    match separator {
        PowerlineSeparator::HardRight => {
            path.move_to(point(left - overlap, top));
            path.line_to(point(right, middle));
            path.line_to(point(left - overlap, bottom));
            path.close();
        }
        PowerlineSeparator::SoftRight => {
            path.move_to(point(left, top));
            path.line_to(point(right, middle));
            path.line_to(point(left, bottom));
        }
        PowerlineSeparator::HardLeft => {
            path.move_to(point(right + overlap, top));
            path.line_to(point(left, middle));
            path.line_to(point(right + overlap, bottom));
            path.close();
        }
        PowerlineSeparator::SoftLeft => {
            path.move_to(point(right, top));
            path.line_to(point(left, middle));
            path.line_to(point(right, bottom));
        }
        PowerlineSeparator::AngledUpper => {
            path.move_to(point(left - overlap, top));
            path.line_to(point(right, top));
            path.line_to(point(left - overlap, bottom));
            path.close();
        }
        PowerlineSeparator::AngledThin => {
            path.move_to(point(left, bottom));
            path.line_to(point(right, top));
        }
        PowerlineSeparator::AngledLower => {
            path.move_to(point(right + overlap, top));
            path.line_to(point(right + overlap, bottom));
            path.line_to(point(left, bottom));
            path.close();
        }
    }
    path.build().ok()
}

fn color(value: u32) -> Hsla {
    rgb(value).into()
}

fn grid_font(families: &[String], weight: FontWeight, italic: bool) -> Font {
    let mut font = font(
        families
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_FONT.into()),
    );
    if families.len() > 1 {
        font.fallbacks = Some(FontFallbacks::from_fonts(families[1..].to_vec()));
    }
    font.features = FontFeatures::disable_ligatures();
    font.weight = weight;
    font.style = if italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    font
}

fn parse_guifont(value: &str) -> ParsedFont {
    let mut families = Vec::new();
    let mut size = DEFAULT_FONT_SIZE;
    let mut width = 0.0;
    let mut weight = FontWeight::NORMAL.0;
    let mut italic = false;
    for configured_font in value.split(',').filter(|font| !font.is_empty()) {
        let mut parts = configured_font.split(':');
        if let Some(family) = parts.next().filter(|family| !family.is_empty()) {
            families.push(family.replace("\\ ", " ").replace('_', " "));
        }
        for option in parts {
            if let Some(height) = option
                .strip_prefix('h')
                .and_then(|height| height.parse().ok())
            {
                size = height;
            } else if let Some(offset) = option
                .strip_prefix('w')
                .and_then(|offset| offset.parse().ok())
            {
                width = offset;
            } else if let Some(value) = option
                .strip_prefix('W')
                .and_then(|value| value.parse::<f32>().ok())
            {
                weight = value.clamp(FontWeight::THIN.0, FontWeight::BLACK.0);
            } else if option == "b" {
                weight = FontWeight::BOLD.0;
            } else if option == "i" {
                italic = true;
            }
        }
    }
    if families.is_empty() {
        families.push(DEFAULT_FONT.into());
    }
    ParsedFont {
        families,
        size,
        width,
        weight,
        italic,
    }
}

fn grid_line_height(ascent: Pixels, descent: Pixels, linespace: i64) -> Pixels {
    (ascent + descent.abs() + px(linespace as f32)).max(px(1.0))
}

fn installed_font_families(configured: Vec<String>, installed: &[String]) -> Vec<String> {
    let mut available = configured
        .into_iter()
        .filter(|family| {
            installed
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(family))
        })
        .collect::<Vec<_>>();
    if available.is_empty() {
        available.push(DEFAULT_FONT.into());
    }
    available
}

fn mouse_button(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
        MouseButton::Navigate(_) => "x1",
    }
}

fn modifiers(modifiers: Modifiers) -> String {
    let mut result = String::new();
    if modifiers.control {
        result.push('C');
    }
    if modifiers.alt {
        result.push('A');
    }
    if modifiers.shift {
        result.push('S');
    }
    if modifiers.platform {
        result.push('D');
    }
    result
}

fn encode_key(key: &Keystroke) -> Option<String> {
    let special = match key.key.as_str() {
        "enter" => Some("CR"),
        "backspace" => Some("BS"),
        "delete" => Some("Del"),
        "escape" => Some("Esc"),
        "tab" => Some("Tab"),
        "up" => Some("Up"),
        "down" => Some("Down"),
        "left" => Some("Left"),
        "right" => Some("Right"),
        "pageup" => Some("PageUp"),
        "pagedown" => Some("PageDown"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "insert" => Some("Insert"),
        key if key.starts_with('f')
            && !key[1..].is_empty()
            && key[1..].chars().all(|ch| ch.is_ascii_digit()) =>
        {
            Some(key)
        }
        _ => None,
    };
    let modified = key.modifiers.control || key.modifiers.alt || key.modifiers.platform;
    if special.is_none() && !modified {
        return None;
    }

    let mut parts = Vec::new();
    if key.modifiers.control {
        parts.push("C");
    }
    if key.modifiers.alt {
        parts.push("M");
    }
    if key.modifiers.shift {
        parts.push("S");
    }
    if key.modifiers.platform {
        parts.push("D");
    }
    parts.push(special.unwrap_or(&key.key));
    Some(format!("<{}>", parts.join("-")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<Cell> {
        vec![Cell {
            text: text.into(),
            highlight: 0,
        }]
    }

    #[test]
    fn recognizes_powerline_separators() {
        assert_eq!(
            powerline_separator("\u{e0b0}"),
            Some(PowerlineSeparator::HardRight)
        );
        assert_eq!(
            powerline_separator("\u{e0b1}"),
            Some(PowerlineSeparator::SoftRight)
        );
        assert_eq!(
            powerline_separator("\u{e0b2}"),
            Some(PowerlineSeparator::HardLeft)
        );
        assert_eq!(
            powerline_separator("\u{e0b3}"),
            Some(PowerlineSeparator::SoftLeft)
        );
        assert_eq!(
            powerline_separator("\u{e0ba}"),
            Some(PowerlineSeparator::AngledLower)
        );
        assert_eq!(
            powerline_separator("\u{e0bb}"),
            Some(PowerlineSeparator::AngledThin)
        );
        assert_eq!(
            powerline_separator("\u{e0bc}"),
            Some(PowerlineSeparator::AngledUpper)
        );
        assert_eq!(
            powerline_separator("\u{e0bd}"),
            Some(PowerlineSeparator::AngledThin)
        );
        assert_eq!(powerline_separator("x"), None);
    }

    #[test]
    fn scroll_retarget_preserves_velocity_and_trailing_order() {
        let mut up = ScrollAnimation {
            top: 0,
            bottom: 10,
            left: 0,
            right: 1,
            direction: 1,
            spring: Spring {
                position: 1.0,
                velocity: -3.0,
            },
            trailing: vec![row("a")],
        };
        let next = ScrollRecord {
            top: 0,
            bottom: 10,
            left: 0,
            right: 1,
            rows: 1,
            evicted: vec![row("b")],
        };
        assert!(up.compatible(&next));
        up.absorb(next);
        assert_eq!(up.spring.position, 2.0);
        assert_eq!(up.spring.velocity, -3.0);
        assert_eq!(up.trailing[0][0].text, "a");
        assert_eq!(up.trailing[1][0].text, "b");

        let mut down = ScrollAnimation {
            direction: -1,
            spring: Spring {
                position: -1.0,
                velocity: 3.0,
            },
            trailing: vec![row("b")],
            ..up
        };
        down.absorb(ScrollRecord {
            top: 0,
            bottom: 10,
            left: 0,
            right: 1,
            rows: -1,
            evicted: vec![row("a")],
        });
        assert_eq!(down.spring.position, -2.0);
        assert_eq!(down.spring.velocity, 3.0);
        assert_eq!(down.trailing[0][0].text, "a");
        assert_eq!(down.trailing[1][0].text, "b");
    }

    #[test]
    fn long_cursor_jump_lands_the_leading_edge_first() {
        let mut animation = CursorAnimation::default();
        animation.snap((0.5, 0.5), CursorShape::Block, 100);
        animation.retarget((0.5, 5.5), CursorShape::Block, 100);
        animation.advance(1.0 / 60.0);

        let points = animation.points();
        assert_eq!(points[1].1, 6.0);
        assert_eq!(points[2].1, 6.0);
        assert!(points[0].1 > 0.0 && points[0].1 < 5.0);
        assert!(points[3].1 > 0.0 && points[3].1 < 5.0);
    }

    #[test]
    fn parses_neovim_guifont() {
        assert_eq!(
            parse_guifont(r"TX-02-Variable,FiraCode\ Nerd\ Font:h17:w-1.4:W450:i"),
            ParsedFont {
                families: vec!["TX-02-Variable".into(), "FiraCode Nerd Font".into()],
                size: 17.0,
                width: -1.4,
                weight: 450.0,
                italic: true,
            }
        );
    }

    #[test]
    fn promotes_the_first_installed_guifont() {
        assert_eq!(
            installed_font_families(
                vec!["SF Mono".into(), "Menlo".into(), "monospace".into()],
                &["Menlo".into(), "Monaco".into()],
            ),
            vec!["Menlo"]
        );
    }

    #[test]
    fn line_height_includes_negative_descent() {
        assert_eq!(grid_line_height(px(13.0), px(-4.0), 2), px(19.0));
    }

    #[test]
    fn raw_chords_and_text_take_separate_paths() {
        let plain = Keystroke {
            key: "a".into(),
            key_char: Some("a".into()),
            modifiers: Modifiers::none(),
        };
        let option = Keystroke {
            key: "s".into(),
            key_char: Some("ß".into()),
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::none()
            },
        };
        let enter = Keystroke {
            key: "enter".into(),
            key_char: Some("\n".into()),
            modifiers: Modifiers::none(),
        };

        assert_eq!(encode_key(&plain), None);
        assert_eq!(
            encode_key(&Keystroke {
                key: "f".into(),
                key_char: Some("f".into()),
                modifiers: Modifiers::none(),
            }),
            None
        );
        assert_eq!(encode_key(&option).as_deref(), Some("<M-s>"));
        assert_eq!(encode_key(&enter).as_deref(), Some("<CR>"));
    }
}
