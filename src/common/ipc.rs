// IPC stuff for starting to split things into a client and server model
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// Hardcoded location for the LocalSocket which is used for client-server comms
//pub const SOCKET_PATH: &str = "/var/run/mosaic/ipc";
pub const SOCKET_PATH: &str = "/tmp/mosaic/ipc";

type SessionID = u64;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct Session {
    // Unique ID for this session
    pub id: SessionID,
    // Identifier for the underlying IPC primitive (socket, pipe)
    pub conn_name: String,
    // User configured alias for the session
    pub alias: String,
}

// How do we want to connect to a session?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientType {
    Reader,
    Writer,
}

// Types of messages sent from the client to the server
#[derive(Debug, Serialize, Deserialize)]
pub enum ClientToServerMsg {
    // List which sessions are available
    ListSessions,
    // Create a new session
    CreateSession,
    // Attach to a running session
    AttachToSession(SessionID, ClientType),
    // Force detach
    DetachSession(SessionID),
    // Disconnect from the session we're connected to
    DisconnectFromSession,
}

// Types of messages sent from the server to the client
// @@@ Implement Serialize and Deserialize for this...
#[derive(Debug, Serialize, Deserialize)]
pub enum ServerToClientMsg {
    // Info about a particular session
    SessionInfo(Session),
    //TODO: Implement serialize myself
    // A list of sessions
    //SessionList(HashSet<Session>),
}
