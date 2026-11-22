//! A process on the other end of the hardware service's pipe, as read from the process itself (never from
//! what a message claims). Shared by the service (who is calling?) and the client (who is serving?).

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Client {
    pub pid: u32,
    pub image: PathBuf,
    pub sid: String,
}
