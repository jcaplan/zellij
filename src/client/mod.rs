use ipc::ServerToClientMsg;

use crate::common::ipc;

pub mod boundaries;
pub mod layout;
pub mod panes;
pub mod sessions;
pub mod tab;

pub fn start_client() {
    let mut server_conn = sessions::connect_to_server().unwrap();
    serde_json::to_writer(&mut server_conn, &ipc::ClientToServerMsg::CreateSession).unwrap();
    println!("Sent message");
    let incoming_msg: ipc::ServerToClientMsg = serde_json::from_reader(&mut server_conn).unwrap();
    println!("Received response");
    match incoming_msg {
        ServerToClientMsg::SessionInfo(sess) => {
            println!("Received session info: {:?}", sess);
        }
    }
}
