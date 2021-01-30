use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::{os::unix::prelude::FromRawFd, process::Stdio};

use crate::common::ipc;

use daemonize::Daemonize;
use interprocess::local_socket::LocalSocketListener;

pub fn start_server() {
    let mut stdout;

    println!("Hello server\n\n");

    unsafe {
        stdout = File::from_raw_fd(io::stdout().as_raw_fd());
    }

    println!("My server stdout is: {:?}\n\n", stdout);

    let mut listener = LocalSocketListener::bind(ipc::SOCKET_PATH).unwrap();

    let daemonize = Daemonize::new()
        .stdout(stdout)
        .exit_action(|| {
          println!("Hello daemonized server is listening")});

    daemonize.start();

    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }


    for conn in listener.incoming() {}

    // TODO
}
