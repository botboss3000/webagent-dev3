//! Resource counters describe this process's Linux proc/mount namespace, not cgroup quotas.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub fn unavailable(reason: &str) -> Value {
    json!({"available": false, "reason": reason})
}

fn read_bounded(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take(1_048_577)
        .read_to_string(&mut text)?;
    if text.len() > 1_048_576 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "counter size"));
    }
    Ok(text)
}

#[derive(Clone, Copy, Debug)]
pub struct Cpu {
    total: u64,
    idle: u64,
    cores: usize,
}

pub fn parse_cpu(text: &str) -> Option<Cpu> {
    let mut lines = text.lines();
    let mut fields = lines.next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // Linux guest counters are already included in user/nice; don't count them twice.
    let ticks: Vec<u64> = fields
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if ticks.len() < 4 {
        return None;
    }
    let total = ticks.iter().try_fold(0u64, |sum, n| sum.checked_add(*n))?;
    let idle = ticks[3].checked_add(*ticks.get(4).unwrap_or(&0))?;
    let cores = lines
        .filter(|line| {
            line.split_whitespace().next().is_some_and(|name| {
                name.strip_prefix("cpu")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .count();
    if cores == 0 {
        return None;
    }
    Some(Cpu { total, idle, cores })
}

fn cpu_value(current: Cpu, previous: Option<Cpu>) -> Value {
    let percent = previous
        .filter(|old| old.cores == current.cores)
        .and_then(|old| {
            let total = current.total.checked_sub(old.total)?;
            let idle = current.idle.checked_sub(old.idle)?;
            if total == 0 || idle > total {
                return None;
            }
            Some((total - idle) as f64 * 100.0 / total as f64)
        });
    json!({"available": true, "logical_processors": current.cores,
        "utilization_percent": percent, "utilization_available": percent.is_some(),
        "utilization_reason": if percent.is_some() { Value::Null } else { json!("warming_up_or_counter_reset") }})
}

pub fn parse_memory(text: &str) -> Option<Value> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next()?;
        if !["MemTotal:", "MemAvailable:", "SwapTotal:", "SwapFree:"].contains(&key) {
            continue;
        }
        let value = parts.next()?.parse::<u64>().ok()?.checked_mul(1024)?;
        if parts.next()? != "kB" || parts.next().is_some() || fields.insert(key, value).is_some() {
            return None;
        }
    }
    let total = *fields.get("MemTotal:")?;
    let available = *fields.get("MemAvailable:")?;
    if total == 0 || available > total {
        return None;
    }
    let swap = match (fields.get("SwapTotal:"), fields.get("SwapFree:")) {
        (Some(total), Some(free)) if free <= total => {
            json!({"available":true,"total_bytes":total,"free_bytes":free,"used_bytes":total-free})
        }
        _ => unavailable("swap_counters_unavailable"),
    };
    Some(
        json!({"available":true,"total_bytes":total,"available_bytes":available,
        "used_bytes":total-available,"utilization_percent":(total-available) as f64*100.0/total as f64,"swap":swap}),
    )
}

#[derive(Clone, Copy, Debug)]
pub struct Network {
    rx: u64,
    tx: u64,
    rx_errors: u64,
    tx_errors: u64,
    rx_dropped: u64,
    tx_dropped: u64,
}

pub fn parse_network(text: &str) -> Option<BTreeMap<String, Network>> {
    let mut result = BTreeMap::new();
    let mut lines = text.lines();
    if !lines.next()?.contains("Inter-") || !lines.next()?.contains("bytes") {
        return None;
    }
    for line in lines {
        let (name, values) = line.rsplit_once(':')?;
        let name = name.trim();
        if name.is_empty() || name.len() > 64 || result.len() >= 128 {
            return None;
        }
        let values: Vec<u64> = values
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        if values.len() != 16 {
            return None;
        }
        if result
            .insert(
                name.to_owned(),
                Network {
                    rx: values[0],
                    tx: values[8],
                    rx_errors: values[2],
                    tx_errors: values[10],
                    rx_dropped: values[3],
                    tx_dropped: values[11],
                },
            )
            .is_some()
        {
            return None;
        }
    }
    Some(result)
}

fn network_value(
    current: &BTreeMap<String, Network>,
    old: Option<&BTreeMap<String, Network>>,
    seconds: f64,
) -> Value {
    let interfaces: Vec<Value> = current.iter().map(|(name, n)| {
        let rates = old.and_then(|old| old.get(name)).and_then(|before| {
            if seconds <= 0.0 || !seconds.is_finite() { return None; }
            Some((n.rx.checked_sub(before.rx)? as f64 / seconds, n.tx.checked_sub(before.tx)? as f64 / seconds))
        });
        json!({"name":name,"loopback":name=="lo","rx_bytes":n.rx,"tx_bytes":n.tx,
            "rx_errors":n.rx_errors,"tx_errors":n.tx_errors,"rx_dropped":n.rx_dropped,"tx_dropped":n.tx_dropped,
            "rates_available":rates.is_some(),"rx_bytes_per_second":rates.map(|r|r.0),"tx_bytes_per_second":rates.map(|r|r.1)})
    }).collect();
    json!({"available":true,"interfaces":interfaces})
}

pub fn parse_uptime(text: &str) -> Option<Value> {
    let seconds = text.split_whitespace().next()?.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(json!({"available":true,"seconds":seconds}))
}

fn disk(path: &Path) -> Value {
    let Ok(name) = CString::new(path.as_os_str().as_bytes()) else {
        return unavailable("invalid_mount");
    };
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: NUL-terminated path and valid writable statvfs storage; only initialized on success.
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return unavailable("filesystem_unavailable");
    }
    // SAFETY: statvfs returned success and initialized the entire structure.
    let stat = unsafe { stat.assume_init() };
    let block = stat.f_frsize;
    let sizes = stat
        .f_blocks
        .checked_mul(block)
        .zip(stat.f_bfree.checked_mul(block))
        .zip(stat.f_bavail.checked_mul(block));
    let Some(((total, free), available)) = sizes else {
        return unavailable("filesystem_counter_overflow");
    };
    if total == 0 || free > total || available > free {
        return unavailable("filesystem_counters_invalid");
    }
    json!({"available":true,"total_bytes":total,"free_bytes":free,"available_bytes":available,
        "used_bytes":total-free,"utilization_percent":(total-free) as f64*100.0/total as f64})
}

pub struct Sampler {
    previous_cpu: Option<Cpu>,
    previous_network: Option<BTreeMap<String, Network>>,
    previous_at: Option<Instant>,
    pub proc_root: std::path::PathBuf,
    mounts: Vec<std::path::PathBuf>,
}

impl Sampler {
    pub fn new(mounts: Vec<std::path::PathBuf>) -> Self {
        Self {
            previous_cpu: None,
            previous_network: None,
            previous_at: None,
            proc_root: "/proc".into(),
            mounts,
        }
    }
    pub fn sample(&mut self) -> Value {
        let now = Instant::now();
        let elapsed = self
            .previous_at
            .map(|old| now.duration_since(old).as_secs_f64())
            .unwrap_or(0.0);
        let cpu = read_bounded(&self.proc_root.join("stat"))
            .ok()
            .and_then(|s| parse_cpu(&s));
        let cpu_json = cpu
            .map(|c| cpu_value(c, self.previous_cpu))
            .unwrap_or_else(|| unavailable("cpu_counters_unavailable"));
        let memory = read_bounded(&self.proc_root.join("meminfo"))
            .ok()
            .and_then(|s| parse_memory(&s))
            .unwrap_or_else(|| unavailable("memory_counters_unavailable"));
        let network = read_bounded(&self.proc_root.join("net/dev"))
            .ok()
            .and_then(|s| parse_network(&s));
        let network_json = network
            .as_ref()
            .map(|n| network_value(n, self.previous_network.as_ref(), elapsed))
            .unwrap_or_else(|| unavailable("network_counters_unavailable"));
        let uptime = read_bounded(&self.proc_root.join("uptime"))
            .ok()
            .and_then(|s| parse_uptime(&s))
            .unwrap_or_else(|| unavailable("uptime_unavailable"));
        let disks: Vec<Value> = self
            .mounts
            .iter()
            .map(|path| json!({"mount":path,"usage":disk(path)}))
            .collect();
        self.previous_cpu = cpu;
        self.previous_network = network;
        self.previous_at = Some(now);
        json!({"cpu":cpu_json,"memory":memory,"network":network_json,"uptime":uptime,"disks":{"available":true,"filesystems":disks}})
    }
}

pub fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_delta_excludes_guest_and_detects_reset() {
        let first = parse_cpu("cpu 10 0 10 80 0 0 0 0 999 999\ncpu0 1\n").unwrap();
        let next = parse_cpu("cpu 30 0 10 160 0 0 0 0 999 999\ncpu0 1\n").unwrap();
        assert_eq!(cpu_value(next, Some(first))["utilization_percent"], 20.0);
        assert!(cpu_value(first, Some(next))["utilization_percent"].is_null());
        assert!(!cpu_value(first, None)["utilization_available"]
            .as_bool()
            .unwrap());
        assert!(parse_cpu("cpu 1 2 invalid\ncpu0 1\n").is_none());
    }
    #[test]
    fn memory_units_and_reserved_memory() {
        let m = parse_memory(
            "MemTotal: 1000 kB\nMemAvailable: 250 kB\nSwapTotal: 20 kB\nSwapFree: 5 kB\n",
        )
        .unwrap();
        assert_eq!(m["total_bytes"], 1_024_000);
        assert_eq!(m["used_bytes"], 768_000);
        assert_eq!(m["swap"]["used_bytes"], 15_360);
        for bad in [
            "MemTotal: 10 kB\nMemAvailable: 20 kB\n",
            "MemTotal: 10 B\nMemAvailable: 2 kB\n",
            "MemTotal: 10 kB\n",
        ] {
            assert!(parse_memory(bad).is_none());
        }
    }
    #[test]
    fn network_rates_and_reset_are_explicit() {
        let header = "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n";
        let first =
            parse_network(&(header.to_owned() + "eth0: 100 0 1 2 0 0 0 0 200 0 3 4 0 0 0 0\n"))
                .unwrap();
        let next =
            parse_network(&(header.to_owned() + "eth0: 300 0 1 2 0 0 0 0 600 0 3 4 0 0 0 0\n"))
                .unwrap();
        let value = network_value(&next, Some(&first), 2.0);
        assert_eq!(value["interfaces"][0]["rx_bytes_per_second"], 100.0);
        assert_eq!(value["interfaces"][0]["tx_bytes_per_second"], 200.0);
        assert!(
            network_value(&first, Some(&next), 1.0)["interfaces"][0]["rx_bytes_per_second"]
                .is_null()
        );
        assert!(parse_network("garbage").is_none());
        assert!(parse_network(&(header.to_owned() + "eth0: 1 2\n")).is_none());
    }
    #[test]
    fn uptime_and_missing_mount() {
        for bad in ["NaN", "inf", "-1", ""] {
            assert!(parse_uptime(bad).is_none());
        }
        assert_eq!(parse_uptime("123.5 100.0").unwrap()["seconds"], 123.5);
        assert_eq!(
            disk(Path::new("/missing-monitoring-test-mount"))["available"],
            false
        );
        assert_eq!(disk(Path::new("/"))["available"], true);
    }
    #[test]
    fn failed_counters_do_not_become_zero() {
        let mut sampler = Sampler::new(vec![]);
        sampler.proc_root = "/missing-monitoring-test-proc".into();
        let snapshot = sampler.sample();
        for name in ["cpu", "memory", "network", "uptime"] {
            assert_eq!(snapshot[name]["available"], false);
        }
    }
    #[test]
    fn oversized_proc_counter_is_rejected() {
        let path = std::env::temp_dir().join(format!("monitor-counter-{}", std::process::id()));
        std::fs::write(&path, vec![b'x'; 1_048_577]).unwrap();
        assert_eq!(
            read_bounded(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_file(path).unwrap();
    }
}
