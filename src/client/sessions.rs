use std::env::current_exe;
use std::io::Read;
use std::process::{Command, Stdio};

use crate::common::ipc;

use interprocess::local_socket::LocalSocketStream;
use termion::input::TermRead;

// Initiate a connection to the server. If this fails, we instead start
// the server and give it a pipe so it can tell us when the listen socket
//is open (so we can revert to using the persistent local socket)
pub fn connect_to_server() -> Result<LocalSocketStream, ()> {
    println!("Hello client");
    if let Ok(conn) = LocalSocketStream::connect(ipc::SOCKET_PATH) {
        // Server already started
        Ok(conn)
    } else {
        // We need to start the server
        spawn_server();
        Err(())
    }
}

fn spawn_server() {
    if let Ok(exe_path) = current_exe() {
        let server_proc = Command::new(exe_path)
            .arg("--server")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let mut s = String::new();
        let server_stdout = &mut server_proc.stdout.unwrap();
        for line in server_stdout.read_line().unwrap() {
            if line.len() > 1 {
                print!("server responded with:\n{}\n\n", line);
            }
        }

        if let Ok(conn) = LocalSocketStream::connect(ipc::SOCKET_PATH) {
            // Server already started
            println!("Managed to connect to server.\n\n");
        }

        print!("finished spawning\n\n");
    }
}
