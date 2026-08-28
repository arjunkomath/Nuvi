mod editor;
mod grid;
mod nvim;
mod workspace;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType,
    TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px,
    size,
};
use workspace::WorkspaceWindow;

actions!(
    nuvi,
    [
        Quit,
        NewWorkspace,
        CloseWorkspace,
        OpenFolder,
        SelectWorkspace1,
        SelectWorkspace2,
        SelectWorkspace3,
        SelectWorkspace4,
        SelectWorkspace5,
        SelectWorkspace6,
        SelectWorkspace7,
        SelectWorkspace8,
        SelectWorkspace9,
    ]
);

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-t", NewWorkspace, None),
            KeyBinding::new("cmd-w", CloseWorkspace, None),
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("cmd-1", SelectWorkspace1, None),
            KeyBinding::new("cmd-2", SelectWorkspace2, None),
            KeyBinding::new("cmd-3", SelectWorkspace3, None),
            KeyBinding::new("cmd-4", SelectWorkspace4, None),
            KeyBinding::new("cmd-5", SelectWorkspace5, None),
            KeyBinding::new("cmd-6", SelectWorkspace6, None),
            KeyBinding::new("cmd-7", SelectWorkspace7, None),
            KeyBinding::new("cmd-8", SelectWorkspace8, None),
            KeyBinding::new("cmd-9", SelectWorkspace9, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Nuvi".into(),
                items: vec![
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
        cx.open_window(
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
        cx.activate(true);
    });
}
