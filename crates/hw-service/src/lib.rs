//! The Wiregent hardware service (docs/plans/x17-hw-service-design.md). H1b: read-only.

pub mod controller;
pub mod identity;
pub use wiregent_hw::proto;
#[cfg(windows)]
pub mod pipe;

use controller::Shared;
use wiregent_hw::client_identity::Client;
use proto::{Request, Response};
use std::sync::Mutex;
use std::time::Instant;

/// Answers one request from what the controller published. Pipe threads never touch the firmware.
pub struct Answers {
    pub shared: std::sync::Arc<Shared>,
    pub health: Box<dyn Fn() -> serde_json::Value + Send + Sync>,
    health_cache: Mutex<Option<(Instant, serde_json::Value)>>,
    pub now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl Answers {
    pub fn new(shared: std::sync::Arc<Shared>, health: Box<dyn Fn() -> serde_json::Value + Send + Sync>, now_ms: Box<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Answers { shared, health, health_cache: Mutex::new(None), now_ms }
    }

    pub fn answer(&self, _client: &Client, req: Request) -> Response {
        match req {
            Request::Hello { .. } => Response::Hello { proto: proto::PROTO, build: proto::BUILD.into() },
            Request::Snapshot => {
                self.shared.set_watched(true);
                let latest = self.shared.latest();
                let now = (self.now_ms)();
                Response::Snapshot { stale_ms: latest.as_ref().map(|(t, _)| now.saturating_sub(*t)), snapshot: latest.map(|(_, v)| v) }
            }
            Request::Lists => Response::Lists { lists: self.shared.lists().unwrap_or(serde_json::Value::Null) },
            Request::SetProfile { profile } => {
                let Some(p) = wiregent_hw::control::parse_profile(&profile) else { return Response::Error { message: format!("'{profile}' isn't a mode id") } };
                match self.shared.request_profile(p).recv_timeout(std::time::Duration::from_secs(8)) {
                    Ok(Ok(change)) => Response::ModeChanged { change },
                    Ok(Err(message)) => Response::Error { message },
                    Err(_) => Response::Error { message: "the hardware controller didn't answer in time".into() },
                }
            }
            Request::SetFanBoost { fan, level } => {
                let Some(f) = wiregent_hw::control::parse_profile(&fan) else { return Response::Error { message: format!("'{fan}' isn't a fan id") } };
                match self.shared.request(controller::Cmd::FanBoost { fan: f, level }).recv_timeout(std::time::Duration::from_secs(8)) {
                    Ok(Ok(change)) => Response::BoostChanged { change },
                    Ok(Err(message)) => Response::Error { message },
                    Err(_) => Response::Error { message: "the hardware controller didn't answer in time".into() },
                }
            }
            Request::Health => {
                // Event-log and disk queries are slow: cached for a minute.
                let mut c = self.health_cache.lock().unwrap();
                if c.as_ref().is_none_or(|(at, _)| at.elapsed().as_secs() >= 60) {
                    *c = Some((Instant::now(), (self.health)()));
                }
                Response::Health { health: c.as_ref().map(|(_, v)| v.clone()).unwrap_or_default() }
            }
        }
    }
}
