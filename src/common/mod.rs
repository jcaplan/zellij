pub mod command_is_executing;
pub mod errors;
pub mod input;
pub mod ipc;
pub mod os_input_output;
pub mod pty_bus;
pub mod screen;
pub mod utils;
pub mod wasm_vm;

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, SendError, Sender, SyncSender};
use std::thread;
use std::{cell::RefCell, sync::mpsc::TrySendError};
use std::{collections::HashMap, fs};

use crate::panes::PaneId;
use directories_next::ProjectDirs;
use input::handler::InputState;
use ipmpsc::{Receiver as IpcReceiver, Sender as IpcSender, SharedRingBuffer};
use serde::{Deserialize, Serialize};
use termion::input::TermRead;
use wasm_vm::PluginEnv;
use wasmer::{ChainableNamedResolver, Instance, Module, Store, Value};
use wasmer_wasi::{Pipe, WasiState};

use crate::cli::CliArgs;
use crate::layout::Layout;
use crate::server::start_server;
use command_is_executing::CommandIsExecuting;
use errors::{AppContext, ContextType, ErrorContext, PluginContext, ScreenContext};
use input::handler::input_loop;
use os_input_output::OsApi;
use pty_bus::PtyInstruction;
use screen::{Screen, ScreenInstruction};
use utils::consts::{MOSAIC_IPC_PIPE, MOSAIC_ROOT_PLUGIN_DIR};
use wasm_vm::{mosaic_imports, wasi_stdout, wasi_write_string, PluginInstruction};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerInstruction {
    OpenFile(PathBuf),
    SplitHorizontally,
    SplitVertically,
    MoveFocus,
    NewClient(String),
    ToPty(PtyInstruction),
    ToScreen(ScreenInstruction),
    ClosePluginPane(u32),
    Exit,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientInstruction {
    ToScreen(ScreenInstruction),
    ClosePluginPane(u32),
    Error(String),
    Exit,
}

// FIXME: It would be good to add some more things to this over time
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub input_state: InputState,
}

// FIXME: Make this a method on the big `Communication` struct, so that app_tx can be extracted
// from self instead of being explicitly passed here
pub fn update_state(
    app_tx: &SenderWithContext<AppInstruction>,
    update_fn: impl FnOnce(AppState) -> AppState,
) {
    let (state_tx, state_rx) = channel();

    drop(app_tx.send(AppInstruction::GetState(state_tx)));
    let state = state_rx.recv().unwrap();

    drop(app_tx.send(AppInstruction::SetState(update_fn(state))))
}

pub type ChannelWithContext<T> = (Sender<(T, ErrorContext)>, Receiver<(T, ErrorContext)>);
pub type SyncChannelWithContext<T> = (SyncSender<(T, ErrorContext)>, Receiver<(T, ErrorContext)>);

#[derive(Clone)]
pub enum SenderType<T: Clone> {
    Sender(Sender<(T, ErrorContext)>),
    SyncSender(SyncSender<(T, ErrorContext)>),
}

#[derive(Clone)]
pub struct SenderWithContext<T: Clone> {
    err_ctx: ErrorContext,
    sender: SenderType<T>,
}

impl<T: Clone> SenderWithContext<T> {
    pub fn new(err_ctx: ErrorContext, sender: SenderType<T>) -> Self {
        Self { err_ctx, sender }
    }

    pub fn send(&self, event: T) -> Result<(), SendError<(T, ErrorContext)>> {
        match self.sender {
            SenderType::Sender(ref s) => s.send((event, self.err_ctx)),
            SenderType::SyncSender(ref s) => s.send((event, self.err_ctx)),
        }
    }

    pub fn try_send(&self, event: T) -> Result<(), TrySendError<(T, ErrorContext)>> {
        if let SenderType::SyncSender(ref s) = self.sender {
            s.try_send((event, self.err_ctx))
        } else {
            panic!("try_send can only be called on SyncSenders!")
        }
    }

    pub fn update(&mut self, new_ctx: ErrorContext) {
        self.err_ctx = new_ctx;
    }
}

unsafe impl<T: Clone> Send for SenderWithContext<T> {}
unsafe impl<T: Clone> Sync for SenderWithContext<T> {}

#[derive(Clone)]
pub struct IpcSenderWithContext {
    err_ctx: ErrorContext,
    sender: IpcSender,
}

impl IpcSenderWithContext {
    pub fn new(buffer: SharedRingBuffer) -> Self {
        Self {
            err_ctx: ErrorContext::new(),
            sender: IpcSender::new(buffer),
        }
    }

    // This is expensive. Use this only if a buffer is not available.
    // Otherwise clone the buffer and use `new()`
    pub fn to_server() -> Self {
        Self::new(SharedRingBuffer::open(MOSAIC_IPC_PIPE).unwrap())
    }

    pub fn update(&mut self, ctx: ErrorContext) {
        self.err_ctx = ctx;
    }

    pub fn send<T: Serialize>(&mut self, msg: T) -> ipmpsc::Result<()> {
        self.sender.send(&(self.err_ctx, msg))
    }
}

thread_local!(static OPENCALLS: RefCell<ErrorContext> = RefCell::default());

#[derive(Clone)]
pub enum AppInstruction {
    GetState(Sender<AppState>),
    SetState(AppState),
    Exit,
    Error(String),
    ToPty(PtyInstruction),
    ToScreen(ScreenInstruction),
    ToPlugin(PluginInstruction),
}

impl From<ClientInstruction> for AppInstruction {
    fn from(item: ClientInstruction) -> Self {
        match item {
            ClientInstruction::ToScreen(s) => AppInstruction::ToScreen(s),
            ClientInstruction::Error(e) => AppInstruction::Error(e),
            ClientInstruction::ClosePluginPane(p) => {
                AppInstruction::ToPlugin(PluginInstruction::Unload(p))
            }
            ClientInstruction::Exit => AppInstruction::Exit,
        }
    }
}

pub fn start(mut os_input: Box<dyn OsApi>, opts: CliArgs) {
    let take_snapshot = "\u{1b}[?1049h";
    os_input.unset_raw_mode(0);
    let _ = os_input
        .get_stdout_writer()
        .write(take_snapshot.as_bytes())
        .unwrap();
    let mut app_state = AppState::default();

    let command_is_executing = CommandIsExecuting::new();

    let full_screen_ws = os_input.get_terminal_size_using_fd(0);
    os_input.set_raw_mode(0);
    let (send_screen_instructions, receive_screen_instructions): ChannelWithContext<
        ScreenInstruction,
    > = channel();
    let err_ctx = OPENCALLS.with(|ctx| *ctx.borrow());
    let mut send_screen_instructions =
        SenderWithContext::new(err_ctx, SenderType::Sender(send_screen_instructions));

    let (send_plugin_instructions, receive_plugin_instructions): ChannelWithContext<
        PluginInstruction,
    > = channel();
    let send_plugin_instructions =
        SenderWithContext::new(err_ctx, SenderType::Sender(send_plugin_instructions));

    let (send_app_instructions, receive_app_instructions): SyncChannelWithContext<AppInstruction> =
        sync_channel(500);
    let mut send_app_instructions =
        SenderWithContext::new(err_ctx, SenderType::SyncSender(send_app_instructions));

    let ipc_thread = start_server(os_input.clone(), opts.clone());

    let (client_buffer_path, client_buffer) = SharedRingBuffer::create_temp(8192).unwrap();
    let mut send_server_instructions = IpcSenderWithContext::to_server();
    send_server_instructions
        .send(ServerInstruction::NewClient(client_buffer_path))
        .unwrap();

    #[cfg(not(test))]
    std::panic::set_hook({
        use crate::errors::handle_panic;
        let send_app_instructions = send_app_instructions.clone();
        Box::new(move |info| {
            handle_panic(info, &send_app_instructions);
        })
    });

    let screen_thread = thread::Builder::new()
        .name("screen".to_string())
        .spawn({
            let mut command_is_executing = command_is_executing.clone();
            let os_input = os_input.clone();
            let send_plugin_instructions = send_plugin_instructions.clone();
            let send_app_instructions = send_app_instructions.clone();
            let max_panes = opts.max_panes;

            move || {
                let mut screen = Screen::new(
                    receive_screen_instructions,
                    send_plugin_instructions,
                    send_app_instructions,
                    &full_screen_ws,
                    os_input,
                    max_panes,
                );
                loop {
                    let (event, mut err_ctx) = screen
                        .receiver
                        .recv()
                        .expect("failed to receive event on channel");
                    err_ctx.add_call(ContextType::Screen(ScreenContext::from(&event)));
                    screen.send_app_instructions.update(err_ctx);
                    match event {
                        ScreenInstruction::Pty(pid, vte_event) => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .handle_pty_event(pid, vte_event);
                        }
                        ScreenInstruction::Render => {
                            screen.render();
                        }
                        ScreenInstruction::NewPane(pid) => {
                            screen.get_active_tab_mut().unwrap().new_pane(pid);
                            command_is_executing.done_opening_new_pane();
                        }
                        ScreenInstruction::HorizontalSplit(pid) => {
                            screen.get_active_tab_mut().unwrap().horizontal_split(pid);
                            command_is_executing.done_opening_new_pane();
                        }
                        ScreenInstruction::VerticalSplit(pid) => {
                            screen.get_active_tab_mut().unwrap().vertical_split(pid);
                            command_is_executing.done_opening_new_pane();
                        }
                        ScreenInstruction::WriteCharacter(bytes) => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .write_to_active_terminal(bytes);
                        }
                        ScreenInstruction::ResizeLeft => {
                            screen.get_active_tab_mut().unwrap().resize_left();
                        }
                        ScreenInstruction::ResizeRight => {
                            screen.get_active_tab_mut().unwrap().resize_right();
                        }
                        ScreenInstruction::ResizeDown => {
                            screen.get_active_tab_mut().unwrap().resize_down();
                        }
                        ScreenInstruction::ResizeUp => {
                            screen.get_active_tab_mut().unwrap().resize_up();
                        }
                        ScreenInstruction::MoveFocus => {
                            screen.get_active_tab_mut().unwrap().move_focus();
                        }
                        ScreenInstruction::MoveFocusLeft => {
                            screen.get_active_tab_mut().unwrap().move_focus_left();
                        }
                        ScreenInstruction::MoveFocusDown => {
                            screen.get_active_tab_mut().unwrap().move_focus_down();
                        }
                        ScreenInstruction::MoveFocusRight => {
                            screen.get_active_tab_mut().unwrap().move_focus_right();
                        }
                        ScreenInstruction::MoveFocusUp => {
                            screen.get_active_tab_mut().unwrap().move_focus_up();
                        }
                        ScreenInstruction::ScrollUp => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .scroll_active_terminal_up();
                        }
                        ScreenInstruction::ScrollDown => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .scroll_active_terminal_down();
                        }
                        ScreenInstruction::ClearScroll => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .clear_active_terminal_scroll();
                        }
                        ScreenInstruction::CloseFocusedPane => {
                            screen.get_active_tab_mut().unwrap().close_focused_pane();
                            command_is_executing.done_closing_pane();
                            screen.render();
                        }
                        ScreenInstruction::SetSelectable(id, selectable) => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .set_pane_selectable(id, selectable);
                            // FIXME: Is this needed?
                            screen.render();
                        }
                        ScreenInstruction::SetMaxHeight(id, max_height) => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .set_pane_max_height(id, max_height);
                        }
                        ScreenInstruction::SetInvisibleBorders(id, invisible_borders) => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .set_pane_invisible_borders(id, invisible_borders);
                            screen.render();
                        }
                        ScreenInstruction::ClosePane(id) => {
                            screen.get_active_tab_mut().unwrap().close_pane(id);
                            command_is_executing.done_closing_pane();
                            screen.render();
                        }
                        ScreenInstruction::ToggleActiveTerminalFullscreen => {
                            screen
                                .get_active_tab_mut()
                                .unwrap()
                                .toggle_active_pane_fullscreen();
                        }
                        ScreenInstruction::NewTab(pane_id) => {
                            screen.new_tab(pane_id);
                            command_is_executing.done_opening_new_pane();
                        }
                        ScreenInstruction::SwitchTabNext => screen.switch_tab_next(),
                        ScreenInstruction::SwitchTabPrev => screen.switch_tab_prev(),
                        ScreenInstruction::CloseTab => {
                            screen.close_tab();
                            command_is_executing.done_closing_pane();
                        }
                        ScreenInstruction::ApplyLayout((layout, new_pane_pids)) => {
                            screen.apply_layout(Layout::new(layout), new_pane_pids);
                            command_is_executing.done_opening_new_pane();
                        }
                        ScreenInstruction::Exit => {
                            break;
                        }
                    }
                }
            }
        })
        .unwrap();

    let wasm_thread = thread::Builder::new()
        .name("wasm".to_string())
        .spawn({
            let mut send_screen_instructions = send_screen_instructions.clone();
            let mut send_app_instructions = send_app_instructions.clone();

            let store = Store::default();
            let mut plugin_id = 0;
            let mut plugin_map = HashMap::new();

            move || loop {
                let (event, mut err_ctx) = receive_plugin_instructions
                    .recv()
                    .expect("failed to receive event on channel");
                err_ctx.add_call(ContextType::Plugin(PluginContext::from(&event)));
                send_screen_instructions.update(err_ctx);
                send_app_instructions.update(err_ctx);
                match event {
                    PluginInstruction::Load(pid_tx, path) => {
                        let project_dirs =
                            ProjectDirs::from("org", "Mosaic Contributors", "Mosaic").unwrap();
                        let plugin_dir = project_dirs.data_dir().join("plugins/");
                        let root_plugin_dir = Path::new(MOSAIC_ROOT_PLUGIN_DIR);
                        let wasm_bytes = fs::read(&path)
                            .or_else(|_| fs::read(&path.with_extension("wasm")))
                            .or_else(|_| fs::read(&plugin_dir.join(&path).with_extension("wasm")))
                            .or_else(|_| {
                                fs::read(&root_plugin_dir.join(&path).with_extension("wasm"))
                            })
                            .unwrap_or_else(|_| panic!("cannot find plugin {}", &path.display()));

                        // FIXME: Cache this compiled module on disk. I could use `(de)serialize_to_file()` for that
                        let module = Module::new(&store, &wasm_bytes).unwrap();

                        let output = Pipe::new();
                        let input = Pipe::new();
                        let mut wasi_env = WasiState::new("mosaic")
                            .env("CLICOLOR_FORCE", "1")
                            .preopen(|p| {
                                p.directory(".") // FIXME: Change this to a more meaningful dir
                                    .alias(".")
                                    .read(true)
                                    .write(true)
                                    .create(true)
                            })
                            .unwrap()
                            .stdin(Box::new(input))
                            .stdout(Box::new(output))
                            .finalize()
                            .unwrap();

                        let wasi = wasi_env.import_object(&module).unwrap();

                        let plugin_env = PluginEnv {
                            plugin_id,
                            send_screen_instructions: send_screen_instructions.clone(),
                            send_app_instructions: send_app_instructions.clone(),
                            wasi_env,
                        };

                        let mosaic = mosaic_imports(&store, &plugin_env);
                        let instance = Instance::new(&module, &mosaic.chain_back(wasi)).unwrap();

                        let start = instance.exports.get_function("_start").unwrap();

                        // This eventually calls the `.init()` method
                        start.call(&[]).unwrap();

                        plugin_map.insert(plugin_id, (instance, plugin_env));
                        pid_tx.send(plugin_id).unwrap();
                        plugin_id += 1;
                    }
                    PluginInstruction::Draw(buf_tx, pid, rows, cols) => {
                        let (instance, plugin_env) = plugin_map.get(&pid).unwrap();

                        let draw = instance.exports.get_function("draw").unwrap();

                        draw.call(&[Value::I32(rows as i32), Value::I32(cols as i32)])
                            .unwrap();

                        buf_tx.send(wasi_stdout(&plugin_env.wasi_env)).unwrap();
                    }
                    // FIXME: Deduplicate this with the callback below!
                    PluginInstruction::Input(pid, input_bytes) => {
                        let (instance, plugin_env) = plugin_map.get(&pid).unwrap();

                        let handle_key = instance.exports.get_function("handle_key").unwrap();
                        for key in input_bytes.keys() {
                            if let Ok(key) = key {
                                wasi_write_string(
                                    &plugin_env.wasi_env,
                                    &serde_json::to_string(&key).unwrap(),
                                );
                                handle_key.call(&[]).unwrap();
                            }
                        }

                        drop(send_screen_instructions.send(ScreenInstruction::Render));
                    }
                    PluginInstruction::GlobalInput(input_bytes) => {
                        // FIXME: Set up an event subscription system, and timed callbacks
                        for (instance, plugin_env) in plugin_map.values() {
                            let handler =
                                instance.exports.get_function("handle_global_key").unwrap();
                            for key in input_bytes.keys() {
                                if let Ok(key) = key {
                                    wasi_write_string(
                                        &plugin_env.wasi_env,
                                        &serde_json::to_string(&key).unwrap(),
                                    );
                                    handler.call(&[]).unwrap();
                                }
                            }
                        }

                        drop(send_screen_instructions.send(ScreenInstruction::Render));
                    }
                    PluginInstruction::Unload(pid) => drop(plugin_map.remove(&pid)),
                    PluginInstruction::Exit => break,
                }
            }
        })
        .unwrap();

    let _stdin_thread = thread::Builder::new()
        .name("stdin_handler".to_string())
        .spawn({
            let send_screen_instructions = send_screen_instructions.clone();
            let send_plugin_instructions = send_plugin_instructions.clone();
            let send_app_instructions = send_app_instructions.clone();
            let os_input = os_input.clone();
            move || {
                input_loop(
                    os_input,
                    command_is_executing,
                    send_screen_instructions,
                    send_plugin_instructions,
                    send_app_instructions,
                )
            }
        });

    let router_thread = thread::Builder::new()
        .name("router".to_string())
        .spawn({
            let recv_client_instructions = IpcReceiver::new(client_buffer);
            move || loop {
                let (err_ctx, instruction): (ErrorContext, ClientInstruction) =
                    recv_client_instructions.recv().unwrap();
                send_app_instructions.update(err_ctx);
                match instruction {
                    ClientInstruction::Exit => break,
                    _ => {
                        send_app_instructions
                            .send(AppInstruction::from(instruction))
                            .unwrap();
                    }
                }
            }
        })
        .unwrap();

    #[warn(clippy::never_loop)]
    loop {
        let (app_instruction, mut err_ctx) = receive_app_instructions
            .recv()
            .expect("failed to receive app instruction on channel");

        err_ctx.add_call(ContextType::App(AppContext::from(&app_instruction)));
        send_screen_instructions.update(err_ctx);
        send_server_instructions.update(err_ctx);
        match app_instruction {
            AppInstruction::GetState(state_tx) => drop(state_tx.send(app_state.clone())),
            AppInstruction::SetState(state) => app_state = state,
            AppInstruction::Exit => break,
            AppInstruction::Error(backtrace) => {
                let _ = send_server_instructions.send(ServerInstruction::Exit);
                let _ = send_screen_instructions.send(ScreenInstruction::Exit);
                let _ = send_plugin_instructions.send(PluginInstruction::Exit);
                let _ = screen_thread.join();
                let _ = wasm_thread.join();
                let _ = ipc_thread.join();
                //let _ = router_thread.join();
                os_input.unset_raw_mode(0);
                let goto_start_of_last_line = format!("\u{1b}[{};{}H", full_screen_ws.rows, 1);
                let error = format!("{}\n{}", goto_start_of_last_line, backtrace);
                let _ = os_input
                    .get_stdout_writer()
                    .write(error.as_bytes())
                    .unwrap();
                std::process::exit(1);
            }
            AppInstruction::ToScreen(instruction) => {
                send_screen_instructions.send(instruction).unwrap();
            }
            AppInstruction::ToPlugin(instruction) => {
                send_plugin_instructions.send(instruction).unwrap();
            }
            AppInstruction::ToPty(instruction) => {
                let _ = send_server_instructions.send(ServerInstruction::ToPty(instruction));
            }
        }
    }

    let _ = send_server_instructions.send(ServerInstruction::Exit);
    let _ = send_screen_instructions.send(ScreenInstruction::Exit);
    let _ = send_plugin_instructions.send(PluginInstruction::Exit);
    screen_thread.join().unwrap();
    wasm_thread.join().unwrap();
    ipc_thread.join().unwrap();
    router_thread.join().unwrap();

    // cleanup();
    let reset_style = "\u{1b}[m";
    let show_cursor = "\u{1b}[?25h";
    let restore_snapshot = "\u{1b}[?1049l";
    let goto_start_of_last_line = format!("\u{1b}[{};{}H", full_screen_ws.rows, 1);
    let goodbye_message = format!(
        "{}\n{}{}{}Bye from Mosaic!",
        goto_start_of_last_line, restore_snapshot, reset_style, show_cursor
    );

    os_input.unset_raw_mode(0);
    let _ = os_input
        .get_stdout_writer()
        .write(goodbye_message.as_bytes())
        .unwrap();
    os_input.get_stdout_writer().flush().unwrap();
}
