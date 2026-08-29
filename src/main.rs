mod editor;
mod grid;
mod nvim;
mod workspace;

use gpui::{
    Action, App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType,
    TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px,
    size,
};
use workspace::WorkspaceWindow;

actions!(
    nuvi,
    [Quit, NewWorkspace, CloseWorkspace, OpenFolder, OpenSettings]
);

#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = nuvi, no_json)]
pub struct SelectWorkspace {
    pub index: usize,
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-t", NewWorkspace, None),
            KeyBinding::new("cmd-w", CloseWorkspace, None),
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
        ]);
        cx.bind_keys((1..=9).map(|digit| {
            KeyBinding::new(
                &format!("cmd-{digit}"),
                SelectWorkspace { index: digit - 1 },
                None,
            )
        }));
        cx.set_menus(vec![
            Menu {
                name: "Nuvi".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit Nuvi", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Workspace", NewWorkspace),
                    MenuItem::action("Open Folder…", OpenFolder),
                    MenuItem::separator(),
                    MenuItem::action("Close Workspace", CloseWorkspace),
                ],
            },
        ]);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.0), px(720.0)), cx);
        let workspace_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: None,
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.0), px(16.0))),
                    }),
                    window_background: WindowBackgroundAppearance::Blurred,
                    ..Default::default()
                },
                move |window, cx| {
                    let workspace = cx.new(|cx| WorkspaceWindow::new(window, args, cx));
                    WorkspaceWindow::bind_window(&workspace, window, cx);
                    workspace
                },
            )
            .unwrap();
        cx.on_action(move |_: &Quit, cx| {
            cx.defer(move |cx| {
                let _ = workspace_window.update(cx, |workspace, window, cx| {
                    if workspace.request_window_close(window, cx) {
                        window.remove_window();
                    }
                });
            });
        });
        cx.activate(true);
    });
}
