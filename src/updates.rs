//! Trait `UpdateBackend` + impl Apt + impl Mock.
//!
//! Les méthodes sont bloquantes : l'appelant les exécute via `spawn_blocking`.

use std::{
    collections::HashMap,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

const APT: &str = "/usr/bin/apt";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const JOURNALCTL: &str = "/usr/bin/journalctl";
const UPGRADE_UNIT: &str = "apt-upgrade.service";
const REBOOT_REQUIRED: &str = "/run/reboot-required";
const LIST_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Package {
    name: String,
    current: String,
    candidate: String,
}

#[derive(Serialize)]
pub struct UpdateList {
    count: usize,
    packages: Vec<Package>,
    reboot_required: bool,
    checked_at: u64,
}

impl UpdateList {
    fn new(packages: Vec<Package>, reboot_required: bool, checked_at: u64) -> Self {
        Self {
            count: packages.len(),
            packages,
            reboot_required,
            checked_at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Running,
    Done,
    Failed,
}

#[derive(Serialize)]
pub struct UpdateStatus {
    state: State,
    log: Vec<String>,
}

pub enum UpdateError {
    AlreadyRunning,
    Failed(String),
}

impl From<String> for UpdateError {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

pub trait UpdateBackend: Send + Sync {
    fn list(&self) -> Result<UpdateList, UpdateError>;
    fn apply(&self) -> Result<(), UpdateError>;
    fn status(&self) -> Result<UpdateStatus, UpdateError>;
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// --- Apt -------------------------------------------------------------------

struct ListCache {
    at: Instant,
    /// `StateChangeTimestampMonotonic` de l'unité de MAJ au moment du remplissage :
    /// s'il change, une MAJ a démarré ou s'est terminée et le cache est périmé.
    upgrade_stamp: String,
    packages: Vec<Package>,
    checked_at: u64,
}

#[derive(Default)]
pub struct AptBackend {
    cache: Mutex<Option<ListCache>>,
    apply_lock: Mutex<()>,
    launched: AtomicBool,
}

impl AptBackend {
    fn state(&self) -> Result<State, String> {
        let props = unit_properties("ActiveState,Result,InactiveExitTimestampMonotonic")?;
        let get = |k: &str| props.get(k).map_or("", String::as_str);
        let has_run = self.launched.load(Ordering::Relaxed)
            || !matches!(get("InactiveExitTimestampMonotonic"), "" | "0");
        Ok(map_state(get("ActiveState"), get("Result"), has_run))
    }
}

impl UpdateBackend for AptBackend {
    fn list(&self) -> Result<UpdateList, UpdateError> {
        let upgrade_stamp = unit_properties("StateChangeTimestampMonotonic")
            .ok()
            .and_then(|mut p| p.remove("StateChangeTimestampMonotonic"))
            .unwrap_or_default();

        let mut cache = lock(&self.cache);
        let entry = match cache.take() {
            Some(c) if c.at.elapsed() < LIST_CACHE_TTL && c.upgrade_stamp == upgrade_stamp => c,
            _ => {
                let output = run(Command::new(APT)
                    .args(["list", "--upgradable"])
                    .env("LANG", "C")
                    .env("LC_ALL", "C")
                    .stderr(Stdio::null()))?;
                ListCache {
                    at: Instant::now(),
                    upgrade_stamp,
                    packages: parse_apt_list(&output),
                    checked_at: now_unix(),
                }
            }
        };
        let list = UpdateList::new(
            entry.packages.clone(),
            Path::new(REBOOT_REQUIRED).exists(),
            entry.checked_at,
        );
        *cache = Some(entry);
        Ok(list)
    }

    fn apply(&self) -> Result<(), UpdateError> {
        let _guard = lock(&self.apply_lock);
        if self.state()? == State::Running {
            return Err(UpdateError::AlreadyRunning);
        }
        run(Command::new(SYSTEMCTL).args(["start", "--no-block", UPGRADE_UNIT]))?;
        self.launched.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn status(&self) -> Result<UpdateStatus, UpdateError> {
        let state = self.state()?;
        let log = match run(Command::new(JOURNALCTL).args([
            "-u",
            UPGRADE_UNIT,
            "-n",
            "50",
            "-o",
            "cat",
            "--no-pager",
        ])) {
            Ok(out) => out.lines().map(str::to_owned).collect(),
            Err(e) => vec![format!("journal indisponible : {e}")],
        };
        Ok(UpdateStatus { state, log })
    }
}

/// Exécute une commande constante (jamais de shell) et renvoie sa sortie standard.
fn run(cmd: &mut Command) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program} : {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{program} : {} {}", out.status, stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn unit_properties(props: &str) -> Result<HashMap<String, String>, String> {
    let out = run(Command::new(SYSTEMCTL).args(["show", "-p", props, UPGRADE_UNIT]))?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect())
}

fn map_state(active_state: &str, result: &str, has_run: bool) -> State {
    match active_state {
        "activating" | "active" => State::Running,
        "failed" => State::Failed,
        "inactive" if result == "success" && has_run => State::Done,
        _ => State::Idle,
    }
}

/// Parse la sortie de `apt list --upgradable` ; les lignes inattendues sont ignorées.
/// Format : `nom/suite version arch [upgradable from: ancienne]`.
fn parse_apt_list(output: &str) -> Vec<Package> {
    output.lines().filter_map(parse_apt_line).collect()
}

fn parse_apt_line(line: &str) -> Option<Package> {
    if line.starts_with("Listing...") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let (name, _suite) = parts.next()?.split_once('/')?;
    let candidate = parts.next()?;
    let _arch = parts.next()?;
    if parts.next()? != "[upgradable" || parts.next()? != "from:" {
        return None;
    }
    let current = parts.next()?.strip_suffix(']')?;
    if parts.next().is_some() || name.is_empty() || current.is_empty() {
        return None;
    }
    Some(Package {
        name: name.to_owned(),
        current: current.to_owned(),
        candidate: candidate.to_owned(),
    })
}

// --- Mock ------------------------------------------------------------------

const MOCK_DURATION: Duration = Duration::from_secs(10);
const MOCK_PACKAGES: [(&str, &str, &str); 3] = [
    ("openssl", "3.5.1-1", "3.5.1-2"),
    ("curl", "8.14.1-2", "8.14.1-2+deb13u1"),
    ("linux-image-amd64", "6.12.38-1", "6.12.41-1"),
];
const MOCK_LOG: [&str; 10] = [
    "Reading package lists...",
    "Building dependency tree...",
    "Reading state information...",
    "Calculating upgrade...",
    "The following packages will be upgraded:",
    "  curl linux-image-amd64 openssl",
    "3 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.",
    "Unpacking openssl (3.5.1-2) over (3.5.1-1) ...",
    "Setting up curl (8.14.1-2+deb13u1) ...",
    "Setting up linux-image-amd64 (6.12.41-1) ...",
];

/// Backend factice : `apply()` simule 10 s de MAJ, après quoi la liste est vide
/// et un redémarrage est requis.
#[derive(Default)]
pub struct MockBackend {
    started: Mutex<Option<Instant>>,
}

impl MockBackend {
    fn elapsed(&self) -> Option<Duration> {
        lock(&self.started).map(|t| t.elapsed())
    }
}

impl UpdateBackend for MockBackend {
    fn list(&self) -> Result<UpdateList, UpdateError> {
        let done = self.elapsed().is_some_and(|e| e >= MOCK_DURATION);
        let packages = if done {
            Vec::new()
        } else {
            MOCK_PACKAGES
                .iter()
                .map(|(name, current, candidate)| Package {
                    name: (*name).into(),
                    current: (*current).into(),
                    candidate: (*candidate).into(),
                })
                .collect()
        };
        Ok(UpdateList::new(packages, done, now_unix()))
    }

    fn apply(&self) -> Result<(), UpdateError> {
        let mut started = lock(&self.started);
        if started.is_some_and(|t| t.elapsed() < MOCK_DURATION) {
            return Err(UpdateError::AlreadyRunning);
        }
        *started = Some(Instant::now());
        Ok(())
    }

    fn status(&self) -> Result<UpdateStatus, UpdateError> {
        let (state, lines) = match self.elapsed() {
            None => (State::Idle, 0),
            Some(e) if e < MOCK_DURATION => {
                let n = (e.as_millis() * MOCK_LOG.len() as u128 / MOCK_DURATION.as_millis()) + 1;
                (State::Running, n as usize)
            }
            Some(_) => (State::Done, MOCK_LOG.len()),
        };
        Ok(UpdateStatus {
            state,
            log: MOCK_LOG[..lines].iter().map(|l| (*l).into()).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, current: &str, candidate: &str) -> Package {
        Package {
            name: name.into(),
            current: current.into(),
            candidate: candidate.into(),
        }
    }

    #[test]
    fn parse_normal_lines() {
        let out = "openssl/stable-security 3.5.1-2 amd64 [upgradable from: 3.5.1-1]\n\
                   tzdata/stable,stable-updates 2025b-4+deb13u1 all [upgradable from: 2025b-4]\n\
                   vim/stable 2:9.1.1230-2 amd64 [upgradable from: 2:9.1.1230-1]\n";
        assert_eq!(
            parse_apt_list(out),
            [
                pkg("openssl", "3.5.1-1", "3.5.1-2"),
                pkg("tzdata", "2025b-4", "2025b-4+deb13u1"),
                pkg("vim", "2:9.1.1230-1", "2:9.1.1230-2"),
            ]
        );
    }

    #[test]
    fn parse_skips_listing_line() {
        let out = "Listing... Done\nopenssl/stable 3.5.1-2 amd64 [upgradable from: 3.5.1-1]\n";
        assert_eq!(parse_apt_list(out), [pkg("openssl", "3.5.1-1", "3.5.1-2")]);
        assert!(parse_apt_list("Listing...\n").is_empty());
    }

    #[test]
    fn parse_empty_output() {
        assert!(parse_apt_list("").is_empty());
        assert!(parse_apt_list("\n\n").is_empty());
    }

    #[test]
    fn parse_ignores_malformed_lines() {
        let out = "garbage\n\
                   nosuite 1.0 amd64 [upgradable from: 0.9]\n\
                   foo/stable 1.0 amd64\n\
                   foo/stable 1.0 amd64 [upgradable from: 0.9\n\
                   foo/stable 1.0 amd64 [installed,upgradable to: 1.1]\n\
                   /stable 1.0 amd64 [upgradable from: 0.9]\n\
                   foo/stable 1.0 amd64 [upgradable from: 0.9] extra\n\
                   bar/stable 2.0 amd64 [upgradable from: 1.0]\n";
        assert_eq!(parse_apt_list(out), [pkg("bar", "1.0", "2.0")]);
    }

    #[test]
    fn unit_state_mapping() {
        assert_eq!(map_state("activating", "success", false), State::Running);
        assert_eq!(map_state("active", "success", true), State::Running);
        assert_eq!(map_state("failed", "exit-code", true), State::Failed);
        assert_eq!(map_state("inactive", "success", true), State::Done);
        assert_eq!(map_state("inactive", "success", false), State::Idle);
        assert_eq!(map_state("inactive", "exit-code", true), State::Idle);
        assert_eq!(map_state("", "", false), State::Idle);
    }

    #[test]
    fn mock_flow() {
        let mock = MockBackend::default();
        assert_eq!(mock.status().ok().map(|s| s.state), Some(State::Idle));
        assert_eq!(mock.list().ok().map(|l| l.count), Some(3));
        assert!(mock.apply().is_ok());
        assert_eq!(mock.status().ok().map(|s| s.state), Some(State::Running));
        assert!(matches!(mock.apply(), Err(UpdateError::AlreadyRunning)));
    }
}
