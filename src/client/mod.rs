pub mod boundaries;
pub mod layout;
pub mod panes;
pub mod sessions;
pub mod tab;

pub fn start_client() {
    let server_conn = sessions::connect_to_server().unwrap();
}
