//! Short-lived download tickets for module sidecar files.
//!
//! A module asks the daemon to prepare a download for a path on its own
//! sidecar. The daemon probes the sidecar and mints a short-lived ticket. The
//! shell consumes the returned URL; the module never receives it. Tickets are
//! kept in memory, are bound to the module that requested them, and expire.
//! They can be fetched more than once within the TTL so browser retries and
//! range resume keep working.

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TICKET_TTL: Duration = Duration::from_secs(300);
const MAX_TICKETS: usize = 256;

#[derive(Clone, Debug)]
pub struct DownloadTicket {
    pub module: String,
    /// Path and query on the module sidecar, for example
    /// `/download?root=%2Fhome%2Feolu&path=%2FDownloads%2Fphoto.jpg`.
    pub path: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size: Option<u64>,
    expires: Instant,
}

impl DownloadTicket {
    pub fn new(
        module: impl Into<String>,
        path: impl Into<String>,
        filename: impl Into<String>,
        content_type: Option<String>,
        size: Option<u64>,
    ) -> Self {
        Self {
            module: module.into(),
            path: path.into(),
            filename: filename.into(),
            content_type,
            size,
            expires: Instant::now(),
        }
    }
}

pub struct DownloadStore {
    tickets: Mutex<HashMap<String, DownloadTicket>>,
}

impl Default for DownloadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadStore {
    pub fn new() -> Self {
        Self {
            tickets: Mutex::new(HashMap::new()),
        }
    }

    pub fn issue(&self, mut ticket: DownloadTicket) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = Instant::now();
        let mut tickets = self.tickets.lock().expect("download ticket lock");
        tickets.retain(|_, entry| entry.expires > now);
        if tickets.len() >= MAX_TICKETS {
            bail!("too many pending downloads; try again in a few minutes");
        }
        ticket.expires = now + TICKET_TTL;
        tickets.insert(id.clone(), ticket);
        Ok(id)
    }

    /// Return a live ticket without consuming it. Browsers may retry a
    /// download URL (resume, interrupted transfer), so the ticket stays valid
    /// until its short TTL expires.
    pub fn get(&self, id: &str) -> Option<DownloadTicket> {
        let now = Instant::now();
        let mut tickets = self.tickets.lock().expect("download ticket lock");
        let live = tickets.get(id).filter(|entry| entry.expires > now).cloned();
        if live.is_none() {
            tickets.remove(id);
        }
        live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket() -> DownloadTicket {
        DownloadTicket {
            module: "files".into(),
            path: "/download?root=%2Ftmp&path=a.txt".into(),
            filename: "a.txt".into(),
            content_type: Some("text/plain".into()),
            size: Some(5),
            expires: Instant::now(),
        }
    }

    #[test]
    fn issues_and_reads_a_ticket() {
        let store = DownloadStore::new();
        let id = store.issue(ticket()).expect("issue");
        let found = store.get(&id).expect("ticket");
        assert_eq!(found.module, "files");
        assert_eq!(found.path, "/download?root=%2Ftmp&path=a.txt");
        assert_eq!(found.filename, "a.txt");
        assert!(store.get(&id).is_some());
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn caps_pending_tickets() {
        let store = DownloadStore::new();
        for _ in 0..MAX_TICKETS {
            store.issue(ticket()).expect("issue");
        }
        assert!(store.issue(ticket()).is_err());
    }
}
