#[cfg(test)]
mod tests;

mod cli;
mod client;
mod common;
mod server;

use client::{boundaries, layout, panes, tab};
use common::{
    command_is_executing, errors, ipc, os_input_output, pty_bus, screen, start, utils, wasm_vm,
    IpcSenderWithContext, ServerInstruction,
};

use structopt::StructOpt;

use crate::cli::CliArgs;
use crate::command_is_executing::CommandIsExecuting;
use crate::os_input_output::get_os_input;
use crate::pty_bus::VteEvent;
use crate::utils::{
    consts::{MOSAIC_TMP_DIR, MOSAIC_TMP_LOG_DIR},
    logging::*,
};

pub fn main() {
    let opts = CliArgs::from_args();
    if let Some(split_dir) = opts.split {
        match split_dir {
            'h' => {
                let mut send_server_instructions = IpcSenderWithContext::new();
                send_server_instructions
                    .send(ServerInstruction::SplitHorizontally)
                    .unwrap();
            }
            'v' => {
                let mut send_server_instructions = IpcSenderWithContext::new();
                send_server_instructions
                    .send(ServerInstruction::SplitVertically)
                    .unwrap();
            }
            _ => {}
        };
    } else if opts.move_focus {
        let mut send_server_instructions = IpcSenderWithContext::new();
        send_server_instructions
            .send(ServerInstruction::MoveFocus)
            .unwrap();
    } else if let Some(file_to_open) = opts.open_file {
        let mut send_server_instructions = IpcSenderWithContext::new();
        send_server_instructions
            .send(ServerInstruction::OpenFile(file_to_open))
            .unwrap();
    } else {
        let os_input = get_os_input();
        atomic_create_dir(MOSAIC_TMP_DIR).unwrap();
        atomic_create_dir(MOSAIC_TMP_LOG_DIR).unwrap();
        start(Box::new(os_input), opts);
    }
}
