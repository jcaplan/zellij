use std::fs::File;
use std::io::{self, prelude::*, BufReader};
use std::os::unix::{io::AsRawFd, prelude::FromRawFd};

use crate::common::ipc;

use daemonize::Daemonize;
use interprocess::local_socket::{LocalSocketListener, LocalSocketStream};

pub fn start_server() {
    let stdout;

    unsafe {
        stdout = File::from_raw_fd(io::stdout().as_raw_fd());
    }

    let listener = LocalSocketListener::bind(ipc::SOCKET_PATH).unwrap();

    let daemonize = Daemonize::new()
        .stdout(stdout)
        .exit_action(|| println!("server running"));

    if daemonize.start().is_ok() {
        event_loop(listener);
    }
    
    loop {
       std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

fn handle_error(conn: io::Result<LocalSocketStream>) -> Option<LocalSocketStream> {
    match conn {
        Ok(val) => Some(val),
        Err(error) => {
            eprintln!("Incoming connection failed: {}", error);
            None
        }
    }
}

fn event_loop(listener: LocalSocketListener) {
    for mut conn in listener.incoming().filter_map(handle_error) {
        let mut conn = BufReader::new(conn);
        let mut buffer = String::new();
        conn.read_line(&mut buffer);
        println!("Client sent: {}", buffer);
    }
}
