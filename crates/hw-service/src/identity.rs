//! Who may talk to the service (design §4). The server reads each client's identity itself (process image and
//! user SID, from the pipe's client PID) and never trusts what a message says about its sender.

use std::path::{Path, PathBuf};

pub use wiregent_hw::client_identity::Client;

/// The programs that may connect: Wiregent, its plugin program and CLI, the key listener (H3), and Alien Cockpit.
pub const CLIENT_IMAGES: &[&str] = &["wiregent.exe", "wiregent-app.exe", "wiregent-alienware.exe", "wiregent-cli.exe", "wiregent-hwkey.exe", "alien-cockpit.exe"];

#[derive(Debug, Clone)]
pub struct Policy {
    /// The SID recorded at install: the person who approved the service.
    pub owner_sid: String,
    /// Folders the client programs may run from (the app's install folder, Program Files\Wiregent\hw).
    pub dirs: Vec<PathBuf>,
    /// Console and test runs only: any folder (still the owner's SID, still a known program name).
    pub any_dir: bool,
    /// Program names that may connect: [`CLIENT_IMAGES`] in the service; a test adds its own.
    pub images: Vec<String>,
}

pub fn client_images() -> Vec<String> {
    CLIENT_IMAGES.iter().map(|s| s.to_string()).collect()
}

fn under(path: &Path, dir: &Path) -> bool {
    let p = path.to_string_lossy().to_ascii_lowercase();
    let d = dir.to_string_lossy().to_ascii_lowercase();
    let d = d.trim_end_matches(['\\', '/']);
    p.starts_with(d) && p[d.len()..].starts_with(['\\', '/'])
}

impl Policy {
    pub fn allow(&self, c: &Client) -> Result<(), String> {
        if !c.sid.eq_ignore_ascii_case(&self.owner_sid) {
            return Err(format!("process {} runs as {}, not as the owner who installed the service", c.pid, c.sid));
        }
        let name = c.image.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !self.images.iter().any(|i| i.eq_ignore_ascii_case(&name)) {
            return Err(format!("{} isn't a Wiregent program", c.image.display()));
        }
        if !self.any_dir && !self.dirs.iter().any(|d| under(&c.image, d)) {
            return Err(format!("{} isn't in Wiregent's install folders", c.image.display()));
        }
        Ok(())
    }
}

/// The pipe's security descriptor: SYSTEM full, the owner read/write, nobody else; protected from inheritance.
pub fn sddl(owner_sid: &str) -> Result<String, String> {
    let ok = owner_sid.starts_with("S-1-5-") && owner_sid.len() < 200 && owner_sid[6..].chars().all(|c| c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(format!("'{owner_sid}' isn't a user SID"));
    }
    Ok(format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{owner_sid})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";

    fn policy() -> Policy {
        Policy { owner_sid: OWNER.into(), dirs: vec![PathBuf::from(r"C:\Users\owner\AppData\Local\Programs\Wiregent"), PathBuf::from(r"C:\Program Files\Wiregent\hw")], any_dir: false, images: client_images() }
    }

    fn client(image: &str, sid: &str) -> Client {
        Client { pid: 42, image: PathBuf::from(image), sid: sid.into() }
    }

    #[test]
    fn only_the_owners_wiregent_programs_in_wiregents_folders() {
        let p = policy();
        assert!(p.allow(&client(r"C:\Users\owner\AppData\Local\Programs\Wiregent\Wiregent.exe", OWNER)).is_ok());
        assert!(p.allow(&client(r"C:\Program Files\Wiregent\hw\wiregent-hwkey.exe", OWNER)).is_ok());
        // C-17: another user, a service account, a look-alike folder, a non-Wiregent program.
        assert!(p.allow(&client(r"C:\Users\owner\AppData\Local\Programs\Wiregent\Wiregent.exe", "S-1-5-21-1-2-3-1001")).is_err());
        assert!(p.allow(&client(r"C:\Program Files\Wiregent\hw\wiregent-hwkey.exe", "S-1-5-18")).is_err());
        assert!(p.allow(&client(r"C:\Users\owner\AppData\Local\Programs\Wiregent-evil\Wiregent.exe", OWNER)).is_err(), "prefix look-alike");
        assert!(p.allow(&client(r"C:\Users\owner\Downloads\Wiregent.exe", OWNER)).is_err());
        assert!(p.allow(&client(r"C:\Users\owner\AppData\Local\Programs\Wiregent\powershell.exe", OWNER)).is_err());
        let dev = Policy { any_dir: true, ..policy() };
        assert!(dev.allow(&client(r"D:\build\debug\wiregent-alienware.exe", OWNER)).is_ok());
        assert!(dev.allow(&client(r"D:\x\notepad.exe", OWNER)).is_err(), "console mode still checks the name");
    }

    /// The SDDL names the owner, never Everyone (WD), Interactive (IU) or Authenticated Users (AU).
    #[test]
    fn the_pipe_sddl_is_owner_only() {
        let s = sddl(OWNER).unwrap();
        assert_eq!(s, format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{OWNER})"));
        for broad in [";WD)", ";IU)", ";AU)", ";BU)"] {
            assert!(!s.contains(broad), "{broad}");
        }
        for bad in ["WD", "S-1-1-0)(A;;GA;;;WD", "", "S-1-5-21-1;"] {
            assert!(sddl(bad).is_err(), "{bad}");
        }
    }
}
