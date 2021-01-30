use std::env::current_exe;
use std::io;
use std::io::Read;
use std::process::{Command, Stdio};

use crate::common::ipc;

use interprocess::local_socket::LocalSocketStream;

// Initiate a connection to the server. If this fails, we instead start
// the server and give it a pipe so it can tell us when the listen socket
//is open (so we can revert to using the persistent local socket)
pub fn connect_to_server() -> io::Result<LocalSocketStream> {
    println!("Hello client");
    if let Ok(conn) = LocalSocketStream::connect(ipc::SOCKET_PATH) {
        // Server already started
        println!("Server already running!");
        Ok(conn)
    } else {
        // We need to start the server
        spawn_server()
    }
}

fn spawn_server() -> io::Result<LocalSocketStream> {
    let mut server_proc = Command::new(current_exe()?)
        .arg("--server")
        .stdout(Stdio::piped())
        .spawn()?;

    let mut s = String::new();
    let server_stdout = &mut server_proc.stdout.take().unwrap();
    loop {
        match server_stdout.read_to_string(&mut s)? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed pipe before sending \"server running\" message",
                ));
            }
            _ => {
                println!("Server responded with: {}", s);
                if s.contains("server running") {
                    println!("Server now running!");
                    break;
                }
            }
        }
    }

    LocalSocketStream::connect(ipc::SOCKET_PATH)
}
