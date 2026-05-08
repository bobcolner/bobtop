//! Process grouping engine — the "intelligent clustering" feature.
//!
//! `GroupMode` selects between four views of the same process list:
//!
//! - **Flat** — one row per process. The original behavior.
//! - **ByExecutable** — collapse all processes with the same `name` into a
//!   single header row. Cheap; answers "is chrome eating my RAM" without
//!   reading 47 lines.
//! - **ByCgroup** — collapse by `cgroup` leaf segment (read from
//!   `/proc/[pid]/cgroup` in the collector). On modern systemd hosts this
//!   is the *useful* clustering — `firefox.service`, `docker-<sha>.scope`,
//!   `user@1000.service` — and containers / k8s pods show up as named
//!   cgroups for free.
//! - **ByContainer** — narrow refinement of `ByCgroup`: only Docker /
//!   Podman / containerd / LXC groups get their own bucket (with names
//!   resolved from runtime metadata), and everything else lumps into a
//!   single `host` group. The right view when "what's running in my
//!   containers?" is the question.
//! - **ByParent** — render as a collapsible parent_pid tree with branch
//!   glyphs (├ │ └). Each non-leaf can be collapsed to hide its subtree.
//!
//! All four modes feed the same `Vec<TableRow>` to the renderer. Headers
//! sort by their aggregates; children inside an expanded group sort by
//! the active sort key. Tree mode sorts roots-then-children separately.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::core::sample::ProcessInfo;
pub use crate::widgets::{TableGroupHeader, TableRow, TableRowMeta};
use crate::widgets::TableSort;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    Flat,
    ByExecutable,
    ByCgroup,
    ByContainer,
    ByParent,
}

impl GroupMode {
    pub fn label(self) -> &'static str {
        match self {
            GroupMode::Flat => "flat",
            GroupMode::ByExecutable => "exec",
            GroupMode::ByCgroup => "cgroup",
            GroupMode::ByContainer => "container",
            GroupMode::ByParent => "tree",
        }
    }

    /// `g` cycles through the modes in this order.
    pub fn next(self) -> Self {
        match self {
            GroupMode::Flat => GroupMode::ByExecutable,
            GroupMode::ByExecutable => GroupMode::ByCgroup,
            GroupMode::ByCgroup => GroupMode::ByContainer,
            GroupMode::ByContainer => GroupMode::ByParent,
            GroupMode::ByParent => GroupMode::Flat,
        }
    }
}

/// Build the row list to render given a sorted+joined process slice and
/// the active grouping mode + expand state.
///
/// Inputs `procs` are assumed already sort-applied and net-joined (i.e.
/// the same data App::processes_sorted contains today). For grouped
/// modes, headers are placed in descending aggregate order so the most
/// expensive groups float to the top regardless of the active per-row
/// sort key.
pub fn build_display(
    procs: &[ProcessInfo],
    mode: GroupMode,
    expanded: &HashSet<String>,
    sort: TableSort,
    descending: bool,
    tree_depth_cap: Option<u8>,
) -> Vec<TableRow> {
    match mode {
        GroupMode::Flat => procs
            .iter()
            .map(|p| {
                TableRow::Item(TableRowMeta {
                    info: p.clone(),
                    depth: 0,
                    is_last_sibling: false,
                    ancestor_continues: Vec::new(),
                })
            })
            .collect(),
        GroupMode::ByExecutable => {
            build_grouped(procs, expanded, sort, descending, |p| p.name.clone())
        }
        GroupMode::ByCgroup => build_grouped(procs, expanded, sort, descending, |p| {
            p.cgroup.clone().unwrap_or_else(|| "(no cgroup)".into())
        }),
        GroupMode::ByContainer => build_grouped(procs, expanded, sort, descending, |p| {
            // Only true containers get their own bucket; host
            // processes share the "host" group so the user can see
            // what's containerized at a glance.
            p.container
                .as_ref()
                .map(|c| c.display())
                .unwrap_or_else(|| "host".into())
        }),
        GroupMode::ByParent => build_tree(procs, expanded, sort, descending, tree_depth_cap),
    }
}

/// Walk the parent tree and report its full depth (max depth a child sits
/// at). 0 = roots only, no descendants in the snapshot. Used to know how
/// far `]` can step before saturating.
pub fn tree_full_depth(procs: &[ProcessInfo]) -> u8 {
    let by_pid: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.parent_pid {
            if pp != p.pid && by_pid.contains_key(&pp) {
                children_of.entry(pp).or_default().push(p.pid);
            }
        }
    }
    fn walk(pid: u32, kids: &HashMap<u32, Vec<u32>>, depth: u8) -> u8 {
        let mut max = depth;
        if let Some(children) = kids.get(&pid) {
            for &c in children {
                let d = walk(c, kids, depth + 1);
                if d > max {
                    max = d;
                }
            }
        }
        max
    }
    let mut deepest = 0;
    for p in procs {
        let is_root = match p.parent_pid {
            None => true,
            Some(pp) => !by_pid.contains_key(&pp),
        };
        if is_root {
            let d = walk(p.pid, &children_of, 0);
            if d > deepest {
                deepest = d;
            }
        }
    }
    deepest
}

/// Compare two `ProcessInfo`s by the active sort key. Used by tree mode to
/// order siblings so pressing `m`/`c`/etc. actually re-shuffles the display
/// instead of leaving everything pid-ascending.
/// Pick the user that owns the most processes in `group`. Ties go to
/// the first-seen owner. Returns `"—"` when the group is empty *or*
/// genuinely mixed (no user > 50 % of the group) so the header reads
/// "this is multi-user, look at the rows" instead of misattributing
/// all of `firefox.service` to one user when init/system services
/// chain through several uids.
fn dominant_user(group: &[ProcessInfo]) -> String {
    if group.is_empty() {
        return "—".to_string();
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in group {
        *counts.entry(p.user.as_str()).or_insert(0) += 1;
    }
    let total = group.len();
    let (top_user, top_count) = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(u, c)| (*u, *c))
        .unwrap_or(("—", 0));
    if top_count * 2 > total {
        top_user.to_string()
    } else {
        "—".to_string()
    }
}

fn cmp_processes(a: &ProcessInfo, b: &ProcessInfo, sort: TableSort) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match sort {
        TableSort::Pid => a.pid.cmp(&b.pid),
        TableSort::Name => a.name.cmp(&b.name),
        TableSort::User => a.user.cmp(&b.user),
        TableSort::Mem => a.mem_rss_bytes.cmp(&b.mem_rss_bytes),
        TableSort::Threads => a.threads.cmp(&b.threads),
        TableSort::Cpu => a
            .cpu_fraction
            .partial_cmp(&b.cpu_fraction)
            .unwrap_or(Ordering::Equal),
        TableSort::NetRx => a
            .net_rx_bytes_per_sec
            .unwrap_or(0.0)
            .partial_cmp(&b.net_rx_bytes_per_sec.unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        TableSort::NetTx => a
            .net_tx_bytes_per_sec
            .unwrap_or(0.0)
            .partial_cmp(&b.net_tx_bytes_per_sec.unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        TableSort::DiskRead => a
            .disk_read_bytes_per_sec
            .unwrap_or(0.0)
            .partial_cmp(&b.disk_read_bytes_per_sec.unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        TableSort::DiskWrite => a
            .disk_write_bytes_per_sec
            .unwrap_or(0.0)
            .partial_cmp(&b.disk_write_bytes_per_sec.unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
    }
}

/// Generic grouper: bucket by a key derived from each process, emit
/// header rows sorted by the aggregate matching the active sort key
/// (so pressing `m` puts the heaviest-mem group on top, NetRx puts
/// the chattiest cgroup on top, etc.), optionally expand to show
/// children. Children inside an expanded group preserve the input
/// order — `procs` is already sorted by the active key, so they're
/// already correct.
fn build_grouped<F>(
    procs: &[ProcessInfo],
    expanded: &HashSet<String>,
    sort: TableSort,
    descending: bool,
    key_fn: F,
) -> Vec<TableRow>
where
    F: Fn(&ProcessInfo) -> String,
{
    let mut buckets: BTreeMap<String, Vec<ProcessInfo>> = BTreeMap::new();
    for p in procs {
        buckets.entry(key_fn(p)).or_default().push(p.clone());
    }

    let mut headers: Vec<(TableGroupHeader, Vec<ProcessInfo>)> = buckets
        .into_iter()
        .map(|(key, group)| {
            let proc_count = group.len();
            let cpu_total: f32 = group.iter().map(|p| p.cpu_fraction).sum();
            let mem_total: u64 = group.iter().map(|p| p.mem_rss_bytes).sum();
            let threads_total: u32 = group.iter().map(|p| p.threads).sum();
            let net_rx: Vec<f64> =
                group.iter().filter_map(|p| p.net_rx_bytes_per_sec).collect();
            let net_tx: Vec<f64> =
                group.iter().filter_map(|p| p.net_tx_bytes_per_sec).collect();
            let disk_r: Vec<f64> =
                group.iter().filter_map(|p| p.disk_read_bytes_per_sec).collect();
            let disk_w: Vec<f64> =
                group.iter().filter_map(|p| p.disk_write_bytes_per_sec).collect();
            let dominant_user = dominant_user(&group);
            let header = TableGroupHeader {
                label: format!("{} ({})", key, proc_count),
                key: key.clone(),
                proc_count,
                threads_total,
                cpu_fraction_total: cpu_total,
                mem_rss_total: mem_total,
                net_rx_total: if net_rx.is_empty() { None } else { Some(net_rx.iter().sum()) },
                net_tx_total: if net_tx.is_empty() { None } else { Some(net_tx.iter().sum()) },
                disk_read_total: if disk_r.is_empty() { None } else { Some(disk_r.iter().sum()) },
                disk_write_total: if disk_w.is_empty() { None } else { Some(disk_w.iter().sum()) },
                dominant_user,
                expanded: expanded.contains(&key),
            };
            (header, group)
        })
        .collect();

    // Sort headers by the aggregate matching the active sort key — so
    // pressing `m` puts the heaviest cgroup on top, `NetRx` puts the
    // chattiest exec name on top, etc. For per-row sort keys that don't
    // aggregate (Pid, User), fall back to alphabetical key order. The
    // `descending` direction follows the user's `r` toggle.
    headers.sort_by(|a, b| {
        use std::cmp::Ordering;
        let ord = match sort {
            TableSort::Mem => a.0.mem_rss_total.cmp(&b.0.mem_rss_total),
            TableSort::Threads => a.0.threads_total.cmp(&b.0.threads_total),
            TableSort::NetRx => a
                .0
                .net_rx_total
                .unwrap_or(0.0)
                .partial_cmp(&b.0.net_rx_total.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::NetTx => a
                .0
                .net_tx_total
                .unwrap_or(0.0)
                .partial_cmp(&b.0.net_tx_total.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskRead => a
                .0
                .disk_read_total
                .unwrap_or(0.0)
                .partial_cmp(&b.0.disk_read_total.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskWrite => a
                .0
                .disk_write_total
                .unwrap_or(0.0)
                .partial_cmp(&b.0.disk_write_total.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            // Per-row keys that don't aggregate cleanly: order by group
            // key (the exec name or cgroup leaf) so listings are stable
            // and discoverable.
            TableSort::Pid | TableSort::Name | TableSort::User => a.0.key.cmp(&b.0.key),
            TableSort::Cpu => a
                .0
                .cpu_fraction_total
                .partial_cmp(&b.0.cpu_fraction_total)
                .unwrap_or(Ordering::Equal),
        };
        if descending { ord.reverse() } else { ord }
    });

    let mut out = Vec::new();
    for (header, children) in headers {
        let expanded = header.expanded;
        out.push(TableRow::Header(header));
        if expanded {
            for p in children {
                out.push(TableRow::Item(TableRowMeta {
                    info: p,
                    depth: 1,
                    is_last_sibling: false,
                    ancestor_continues: vec![false],
                }));
            }
        }
    }
    out
}

/// Build a parent_pid tree. Each process is a row; collapsing a node
/// hides its descendants. `expanded` here uses pid (stringified) as keys;
/// roots are *initially* expanded, so the set tracks COLLAPSED nodes
/// only — sentinel-style. Tracking the smaller set keeps memory tiny
/// and means a fresh App starts with the whole tree visible.
///
/// Roots and each set of siblings are ordered by the active `sort` key —
/// so pressing `m`/`c`/etc. floats heaviest siblings to the top of every
/// subtree. Without this the tree would be permanently pid-ascending and
/// the global sort shortcuts would silently no-op in tree mode.
fn build_tree(
    procs: &[ProcessInfo],
    collapsed: &HashSet<String>,
    sort: TableSort,
    descending: bool,
    depth_cap: Option<u8>,
) -> Vec<TableRow> {
    // Index pid -> ProcessInfo and parent_pid -> children list.
    let by_pid: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.parent_pid {
            // Skip self-parent loops (kernel threads sometimes report this).
            if pp != p.pid && by_pid.contains_key(&pp) {
                children_of.entry(pp).or_default().push(p.pid);
            }
        }
    }
    // Sort each sibling list by the active sort key. `partial_cmp` on f-fields
    // can return None for NaN — `cmp_processes` already coerces those to Equal,
    // so a stable sort gives us a deterministic order.
    let cmp_pid = |a: &u32, b: &u32| {
        let pa = by_pid.get(a).copied();
        let pb = by_pid.get(b).copied();
        let primary = match (pa, pb) {
            (Some(x), Some(y)) => cmp_processes(x, y, sort),
            _ => std::cmp::Ordering::Equal,
        };
        // Match `sort_processes` in app.rs: reverse the primary key for
        // descending mode but keep the PID tiebreaker ascending so equal
        // siblings have a deterministic order across samples (HashMap
        // iteration order is otherwise unstable).
        let primary = if descending { primary.reverse() } else { primary };
        primary.then_with(|| a.cmp(b))
    };
    for v in children_of.values_mut() {
        v.sort_by(cmp_pid);
    }

    // Roots = processes whose parent isn't in the snapshot (incl. pid 1).
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|p| match p.parent_pid {
            None => true,
            Some(pp) => !by_pid.contains_key(&pp),
        })
        .map(|p| p.pid)
        .collect();
    roots.sort_by(cmp_pid);

    let mut out = Vec::new();
    let n = roots.len();
    for (i, root) in roots.into_iter().enumerate() {
        emit_subtree(
            root,
            0,
            i + 1 == n,
            &Vec::new(),
            &by_pid,
            &children_of,
            collapsed,
            depth_cap,
            &mut out,
        );
    }
    out
}

fn emit_subtree(
    pid: u32,
    depth: u8,
    is_last: bool,
    ancestor_continues: &[bool],
    by_pid: &HashMap<u32, &ProcessInfo>,
    children_of: &HashMap<u32, Vec<u32>>,
    collapsed: &HashSet<String>,
    depth_cap: Option<u8>,
    out: &mut Vec<TableRow>,
) {
    let Some(info) = by_pid.get(&pid) else { return };
    out.push(TableRow::Item(TableRowMeta {
        info: (*info).clone(),
        depth,
        is_last_sibling: is_last,
        ancestor_continues: ancestor_continues.to_vec(),
    }));

    let key = pid.to_string();
    if collapsed.contains(&key) {
        return;
    }
    if let Some(cap) = depth_cap {
        if depth >= cap {
            return;
        }
    }
    let Some(kids) = children_of.get(&pid) else { return };
    let mut next_anc = ancestor_continues.to_vec();
    next_anc.push(!is_last);
    let n = kids.len();
    for (i, &child) in kids.iter().enumerate() {
        emit_subtree(
            child,
            depth + 1,
            i + 1 == n,
            &next_anc,
            by_pid,
            children_of,
            collapsed,
            depth_cap,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample::ProcessState;
    use std::time::Instant;

    fn p(pid: u32, parent: Option<u32>, name: &str, cpu: f32, mem_mb: u64, cg: Option<&str>) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: parent,
            name: name.into(),
            cmdline: String::new(),
            user: "u".into(),
            state: ProcessState::Running,
            cpu_fraction: cpu,
            mem_rss_bytes: mem_mb * 1024 * 1024,
            mem_vsz_bytes: 0,
            threads: 1,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: None,
            disk_write_bytes_per_sec: None,
            cgroup: cg.map(|s| s.into()),
            container: None,
        }
    }

    fn with_container(mut info: ProcessInfo, name: &str) -> ProcessInfo {
        use crate::core::sample::{Container, ContainerRuntime};
        info.container = Some(Container {
            runtime: ContainerRuntime::Docker,
            id: format!("{}-id", name),
            name: Some(name.into()),
        });
        info
    }

    fn _ignore() -> Instant { Instant::now() }

    #[test]
    fn flat_mode_yields_one_row_per_process() {
        let ps = vec![p(1, None, "a", 0.0, 0, None), p(2, None, "b", 0.0, 0, None)];
        let rows = build_display(&ps, GroupMode::Flat, &HashSet::new(), TableSort::Cpu, true, None);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], TableRow::Item(_)));
    }

    #[test]
    fn by_executable_collapses_to_one_header_per_name() {
        let ps = vec![
            p(1, None, "chrome", 0.10, 100, None),
            p(2, None, "chrome", 0.20, 200, None),
            p(3, None, "vim", 0.05, 50, None),
        ];
        let rows = build_display(&ps, GroupMode::ByExecutable, &HashSet::new(), TableSort::Cpu, true, None);
        assert_eq!(rows.len(), 2, "two collapsed headers");
        // First header should be chrome (more total CPU).
        match &rows[0] {
            TableRow::Header(h) => {
                assert_eq!(h.key, "chrome");
                assert_eq!(h.proc_count, 2);
                assert!((h.cpu_fraction_total - 0.30).abs() < 1e-6);
                assert_eq!(h.mem_rss_total, 300 * 1024 * 1024);
            }
            _ => panic!("expected Header"),
        }
    }

    #[test]
    fn header_sort_follows_active_sort_key() {
        // Two groups: "small" has 1 GB total mem but tiny CPU; "huge" has
        // 100 MB total mem but lots of CPU. With sort by Mem (desc),
        // "small" must come first; with sort by Cpu (desc), "huge" first.
        let ps = vec![
            p(1, None, "small", 0.01, 1024, None),
            p(2, None, "huge", 0.50, 50, None),
            p(3, None, "huge", 0.40, 50, None),
        ];

        let rows_by_mem =
            build_display(&ps, GroupMode::ByExecutable, &HashSet::new(), TableSort::Mem, true, None);
        match &rows_by_mem[0] {
            TableRow::Header(h) => assert_eq!(h.key, "small", "Mem sort should put small first"),
            _ => panic!("expected Header"),
        }

        let rows_by_cpu =
            build_display(&ps, GroupMode::ByExecutable, &HashSet::new(), TableSort::Cpu, true, None);
        match &rows_by_cpu[0] {
            TableRow::Header(h) => assert_eq!(h.key, "huge", "Cpu sort should put huge first"),
            _ => panic!("expected Header"),
        }

        // Ascending direction reverses both.
        let rows_by_mem_asc =
            build_display(&ps, GroupMode::ByExecutable, &HashSet::new(), TableSort::Mem, false, None);
        match &rows_by_mem_asc[0] {
            TableRow::Header(h) => assert_eq!(h.key, "huge", "Mem asc should flip"),
            _ => panic!("expected Header"),
        }
    }

    #[test]
    fn header_aggregates_threads_total() {
        let mut ps = vec![
            p(1, None, "chrome", 0.1, 100, None),
            p(2, None, "chrome", 0.2, 200, None),
        ];
        ps[0].threads = 5;
        ps[1].threads = 7;
        let rows =
            build_display(&ps, GroupMode::ByExecutable, &HashSet::new(), TableSort::Cpu, true, None);
        match &rows[0] {
            TableRow::Header(h) => assert_eq!(h.threads_total, 12),
            _ => panic!("expected Header"),
        }
    }

    #[test]
    fn by_executable_expands_only_listed_keys() {
        let ps = vec![
            p(1, None, "chrome", 0.10, 100, None),
            p(2, None, "chrome", 0.20, 200, None),
            p(3, None, "vim", 0.05, 50, None),
        ];
        let mut expanded = HashSet::new();
        expanded.insert("chrome".to_string());
        let rows = build_display(&ps, GroupMode::ByExecutable, &expanded, TableSort::Cpu, true, None);
        // chrome header + 2 children + vim header.
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], TableRow::Header(_)));
        assert!(matches!(rows[1], TableRow::Item(_)));
        assert!(matches!(rows[2], TableRow::Item(_)));
        assert!(matches!(rows[3], TableRow::Header(_)));
    }

    #[test]
    fn by_container_buckets_containers_and_lumps_host_processes() {
        let ps = vec![
            with_container(p(1, None, "node", 0.30, 100, None), "web-app"),
            with_container(p(2, None, "node", 0.20, 80, None), "web-app"),
            with_container(p(3, None, "redis", 0.10, 50, None), "redis"),
            p(4, None, "sshd", 0.01, 5, None),
            p(5, None, "bash", 0.01, 5, Some("user-1000.slice")),
        ];
        let rows = build_display(&ps, GroupMode::ByContainer, &HashSet::new(), TableSort::Cpu, true, None);
        let keys: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                TableRow::Header(h) => Some(h.key.as_str()),
                _ => None,
            })
            .collect();
        // Three buckets total: web-app, redis, host (sshd + bash both
        // lack container metadata). Order by CPU desc puts web-app first.
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0], "web-app", "highest-cpu container goes first");
        assert!(keys.contains(&"redis"));
        assert!(keys.contains(&"host"));
        // The host header must aggregate both non-container processes.
        match rows.iter().find_map(|r| match r {
            TableRow::Header(h) if h.key == "host" => Some(h),
            _ => None,
        }) {
            Some(h) => assert_eq!(h.proc_count, 2),
            None => panic!("missing host header"),
        }
    }

    #[test]
    fn by_cgroup_uses_no_cgroup_placeholder_for_none() {
        let ps = vec![
            p(1, None, "x", 0.0, 0, Some("firefox.service")),
            p(2, None, "y", 0.0, 0, None),
        ];
        let rows = build_display(&ps, GroupMode::ByCgroup, &HashSet::new(), TableSort::Cpu, true, None);
        let keys: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                TableRow::Header(h) => Some(h.key.as_str()),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&"firefox.service"));
        assert!(keys.contains(&"(no cgroup)"));
    }

    #[test]
    fn tree_root_then_children_with_depth() {
        // pid 1 has children 100, 200; 100 has child 300.
        let ps = vec![
            p(1, None, "init", 0.0, 0, None),
            p(100, Some(1), "shell", 0.0, 0, None),
            p(200, Some(1), "daemon", 0.0, 0, None),
            p(300, Some(100), "cmd", 0.0, 0, None),
        ];
        let rows = build_display(&ps, GroupMode::ByParent, &HashSet::new(), TableSort::Cpu, true, None);
        let depths: Vec<u8> = rows
            .iter()
            .filter_map(|r| match r {
                TableRow::Item(p) => Some(p.depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1, 2, 1]);
    }

    #[test]
    fn tree_collapsed_node_hides_subtree() {
        let ps = vec![
            p(1, None, "init", 0.0, 0, None),
            p(100, Some(1), "shell", 0.0, 0, None),
            p(300, Some(100), "cmd", 0.0, 0, None),
        ];
        let mut collapsed = HashSet::new();
        collapsed.insert("100".to_string());
        let rows = build_display(&ps, GroupMode::ByParent, &collapsed, TableSort::Cpu, true, None);
        // init + shell — cmd should be hidden under collapsed shell.
        assert_eq!(rows.len(), 2);
    }

    /// Tree-mode sort regression: roots and siblings must obey the active
    /// `TableSort` instead of permanently sitting at pid-ascending. Was
    /// silently broken before — `build_tree` ignored the sort args entirely.
    #[test]
    fn tree_orders_siblings_by_active_sort() {
        // Two roots with distinct CPU values; expected order: 5 (high cpu)
        // then 1 (low cpu) when sort=Cpu desc, and the inverse when desc=false.
        let ps = vec![
            p(1, None, "low", 0.10, 0, None),
            p(5, None, "high", 0.90, 0, None),
        ];

        let rows = build_display(&ps, GroupMode::ByParent, &HashSet::new(), TableSort::Cpu, true, None);
        let pids: Vec<u32> = rows
            .iter()
            .filter_map(|r| match r {
                TableRow::Item(p) => Some(p.info.pid),
                _ => None,
            })
            .collect();
        assert_eq!(pids, vec![5, 1], "Cpu desc should put high-cpu root first");

        let rows_asc =
            build_display(&ps, GroupMode::ByParent, &HashSet::new(), TableSort::Cpu, false, None);
        let pids_asc: Vec<u32> = rows_asc
            .iter()
            .filter_map(|r| match r {
                TableRow::Item(p) => Some(p.info.pid),
                _ => None,
            })
            .collect();
        assert_eq!(pids_asc, vec![1, 5], "Cpu asc should reverse the order");
    }

    /// Tree-mode sort by Mem reorders siblings within an expanded parent.
    #[test]
    fn tree_orders_children_by_mem() {
        let ps = vec![
            p(1, None, "init", 0.0, 0, None),
            p(100, Some(1), "small", 0.0, 50, None),
            p(200, Some(1), "big", 0.0, 500, None),
            p(300, Some(1), "mid", 0.0, 200, None),
        ];
        let rows = build_display(&ps, GroupMode::ByParent, &HashSet::new(), TableSort::Mem, true, None);
        let pids: Vec<u32> = rows
            .iter()
            .filter_map(|r| match r {
                TableRow::Item(p) => Some(p.info.pid),
                _ => None,
            })
            .collect();
        assert_eq!(pids, vec![1, 200, 300, 100], "siblings should be heaviest-mem first");
    }

    /// Tree-mode tiebreaker: equal-keyed siblings must sort by ascending PID
    /// regardless of descending direction, so HashMap iteration order in
    /// the snapshot can't shuffle equal-CPU rows between samples.
    #[test]
    fn tree_breaks_equal_key_ties_by_ascending_pid() {
        // All siblings have identical CPU and mem; only PID differs.
        let ps = vec![
            p(1, None, "init", 0.0, 0, None),
            p(300, Some(1), "c", 0.0, 0, None),
            p(100, Some(1), "a", 0.0, 0, None),
            p(200, Some(1), "b", 0.0, 0, None),
        ];

        for desc in [true, false] {
            let rows =
                build_display(&ps, GroupMode::ByParent, &HashSet::new(), TableSort::Cpu, desc, None);
            let pids: Vec<u32> = rows
                .iter()
                .filter_map(|r| match r {
                    TableRow::Item(p) => Some(p.info.pid),
                    _ => None,
                })
                .collect();
            assert_eq!(
                pids,
                vec![1, 100, 200, 300],
                "ties should ascend by pid; desc={desc}"
            );
        }
    }
}
