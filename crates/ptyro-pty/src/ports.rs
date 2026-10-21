//! Which TCP ports a session's processes listen on (SPEC §10.7): the session leader and all its
//! descendants. Linux reads `/proc`; macOS asks `lsof`. One scan answers for every session.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// For each leader pid, the sorted listening ports of its process tree.
pub fn listening_ports(leaders: &[u32]) -> BTreeMap<u32, Vec<u16>> {
    if leaders.is_empty() {
        return BTreeMap::new();
    }
    let children = process_tree();
    let mut trees: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &leader in leaders {
        let mut all = vec![leader];
        let mut i = 0;
        while i < all.len() {
            if let Some(kids) = children.get(&all[i]) {
                all.extend(kids.iter().copied().filter(|k| !all.contains(k)).collect::<Vec<_>>());
            }
            i += 1;
        }
        trees.insert(leader, all);
    }
    let by_pid = ports_by_pid(&trees.values().flatten().copied().collect::<Vec<_>>());
    trees
        .into_iter()
        .map(|(leader, pids)| {
            let ports: BTreeSet<u16> =
                pids.iter().filter_map(|p| by_pid.get(p)).flatten().copied().collect();
            (leader, ports.into_iter().collect())
        })
        .collect()
}

/// parent pid → child pids.
#[cfg(target_os = "linux")]
fn process_tree() -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return map };
    for e in dir.filter_map(Result::ok) {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        // `pid (comm) state ppid …`; comm may contain spaces and parens, so split at the last `)`.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else { continue };
        if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|p| p.parse::<u32>().ok()) {
            map.entry(ppid).or_default().push(pid);
        }
    }
    map
}

#[cfg(target_os = "linux")]
fn ports_by_pid(pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    // inode → port for every listening TCP socket.
    let mut listening: HashMap<u64, u16> = HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // sl local rem st tx:rx tr:tm retrnsmt uid timeout inode
            if f.len() < 10 || f[3] != "0A" {
                continue;
            }
            let port = f[1].rsplit_once(':').and_then(|(_, p)| u16::from_str_radix(p, 16).ok());
            if let (Some(port), Ok(inode)) = (port, f[9].parse::<u64>()) {
                listening.insert(inode, port);
            }
        }
    }
    let mut out = HashMap::new();
    if listening.is_empty() {
        return out;
    }
    for &pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        let mut ports = Vec::new();
        for fd in fds.filter_map(Result::ok) {
            let Ok(target) = std::fs::read_link(fd.path()) else { continue };
            let t = target.to_string_lossy();
            if let Some(inode) = t
                .strip_prefix("socket:[")
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|i| i.parse::<u64>().ok())
                && let Some(p) = listening.get(&inode)
            {
                ports.push(*p);
            }
        }
        if !ports.is_empty() {
            out.insert(pid, ports);
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn process_tree() -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace().filter_map(|s| s.parse::<u32>().ok());
        if let (Some(pid), Some(ppid)) = (it.next(), it.next()) {
            map.entry(ppid).or_default().push(pid);
        }
    }
    map
}

#[cfg(target_os = "macos")]
fn ports_by_pid(pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let Ok(o) = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN", "-Fpn", "-p", &list])
        .output()
    else {
        return out;
    };
    let mut pid = 0u32;
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix('n')
            && let Some(port) = n.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok())
        {
            out.entry(pid).or_default().push(port);
        }
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_tree() -> HashMap<u32, Vec<u32>> {
    HashMap::new()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ports_by_pid(_pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_listener_in_our_own_tree() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id();
        // Our test process is its own "leader" here.
        let found = listening_ports(&[me]);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(found[&me].contains(&port), "{found:?} lacks {port}");
        }
        assert!(listening_ports(&[]).is_empty());
    }
}
