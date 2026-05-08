//! Container detection from cgroup paths.
//!
//! Parses the leaf segment of a process's cgroup v2 path (already
//! collected by `read_cgroup` in [`crate::collectors::process`]) and recognises the
//! patterns the major runtimes write into systemd's cgroup hierarchy:
//!
//! | Runtime    | Leaf shape                              |
//! |------------|-----------------------------------------|
//! | Docker     | `docker-<id>.scope`                     |
//! | Podman     | `libpod-<id>.scope`                     |
//! | containerd | `cri-containerd-<id>.scope`             |
//! | LXC        | `<name>` (under `/lxc/...`)             |
//!
//! Once we have a `(runtime, id)` we try to resolve a friendly name
//! from filesystem metadata — Docker/Podman both keep a per-container
//! JSON file at a well-known path. Resolution is best-effort: a missing
//! file or stripped permissions leaves `name = None` and the renderer
//! falls back to a runtime-prefixed short id. No daemon socket is
//! contacted, so this works in containers, on read-only filesystems,
//! and without docker / k8s creds.
//!
//! `NameResolver` caches one entry per container id with no TTL: ids
//! are content-addressed (a new container is a new id), so a stale
//! entry can only happen when a container is deleted while we're
//! caching it — harmless for grouping. The cache is bounded to keep
//! memory predictable on hosts with high container churn.

use std::collections::HashMap;
use std::path::Path;

use crate::core::sample::{Container, ContainerRuntime};

/// Recognise a container from the leaf segment of a cgroup path.
///
/// Returns `(runtime, id)` for runtimes the parser knows about. `None`
/// means "not a container" — could be a systemd unit, a user session,
/// or a kernel thread; the caller treats them as belonging to the
/// host group.
pub fn parse_cgroup_leaf(leaf: &str) -> Option<(ContainerRuntime, String)> {
    // `docker-<id>.scope` — Docker on systemd cgroup driver. Older
    // setups used the `cgroupfs` driver and wrote under `/docker/<id>`
    // directly; that case is handled by the path matcher below.
    if let Some(rest) = leaf.strip_prefix("docker-") {
        if let Some(id) = rest.strip_suffix(".scope") {
            if !id.is_empty() {
                return Some((ContainerRuntime::Docker, id.to_string()));
            }
        }
    }
    // `libpod-<id>.scope` — Podman.
    if let Some(rest) = leaf.strip_prefix("libpod-") {
        if let Some(id) = rest.strip_suffix(".scope") {
            if !id.is_empty() {
                return Some((ContainerRuntime::Podman, id.to_string()));
            }
        }
    }
    // `cri-containerd-<id>.scope` — k8s with containerd. Match before
    // the bare `containerd-` prefix so ordering stays correct.
    if let Some(rest) = leaf.strip_prefix("cri-containerd-") {
        if let Some(id) = rest.strip_suffix(".scope") {
            if !id.is_empty() {
                return Some((ContainerRuntime::Containerd, id.to_string()));
            }
        }
    }
    // `containerd-<id>.scope` — bare containerd without CRI wrapper.
    if let Some(rest) = leaf.strip_prefix("containerd-") {
        if let Some(id) = rest.strip_suffix(".scope") {
            if !id.is_empty() {
                return Some((ContainerRuntime::Containerd, id.to_string()));
            }
        }
    }
    None
}

/// Recognise a container from a full cgroup v2 path. Used when the
/// leaf form doesn't match — Docker on the cgroupfs driver writes
/// `/docker/<id>` (no `.scope` suffix) and LXC writes `/lxc/<name>`.
/// Returns `(runtime, id_or_name)`; for LXC the "id" is the human
/// name (LXC doesn't use opaque hashes).
pub fn parse_cgroup_path(full: &str) -> Option<(ContainerRuntime, String)> {
    // /docker/<id>
    if let Some(rest) = full.strip_prefix("/docker/") {
        let id = rest.split('/').next().unwrap_or("");
        if !id.is_empty() {
            return Some((ContainerRuntime::Docker, id.to_string()));
        }
    }
    // /lxc/<name> or /lxc.payload.<name>
    if let Some(rest) = full.strip_prefix("/lxc/") {
        let name = rest.split('/').next().unwrap_or("");
        if !name.is_empty() {
            return Some((ContainerRuntime::Lxc, name.to_string()));
        }
    }
    if let Some(rest) = full.strip_prefix("/lxc.payload.") {
        let name = rest.split('/').next().unwrap_or("");
        if !name.is_empty() {
            return Some((ContainerRuntime::Lxc, name.to_string()));
        }
    }
    None
}

/// LRU-bounded name cache keyed by `(runtime, id)`. Avoids re-reading
/// the same JSON file once per refresh. Capacity is conservative —
/// 256 distinct containers is more than any real host runs at once,
/// and at ~64 bytes per entry the cache caps at ~16 KiB.
pub struct NameResolver {
    cache: HashMap<(ContainerRuntime, String), Option<String>>,
}

impl NameResolver {
    pub fn new() -> Self {
        Self {
            cache: HashMap::with_capacity(64),
        }
    }

    /// Fill `container.name` from the runtime's metadata files when
    /// they're readable. Mutates `container.name` in place. Cheap on
    /// cache hit — no syscalls.
    pub fn resolve(&mut self, container: &mut Container) {
        if container.name.is_some() {
            return;
        }
        let key = (container.runtime, container.id.clone());
        if let Some(cached) = self.cache.get(&key) {
            container.name = cached.clone();
            return;
        }
        let resolved = lookup_name(container.runtime, &container.id);
        self.cache.insert(key, resolved.clone());
        // Bound the cache so a host churning through transient
        // containers doesn't slowly leak. Drop the oldest entries
        // (HashMap iteration order is arbitrary, which is fine for an
        // LRU-ish cap that's only a memory backstop).
        if self.cache.len() > 256 {
            let to_drop: Vec<_> = self.cache.keys().take(64).cloned().collect();
            for k in to_drop {
                self.cache.remove(&k);
            }
        }
        container.name = resolved;
    }
}

impl Default for NameResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort filesystem lookup. Reads the runtime's metadata JSON
/// and pulls out the friendly name. Returns `None` for unsupported
/// runtimes, missing files, permission denied, or malformed JSON —
/// the caller treats `None` the same as "not yet resolved".
fn lookup_name(runtime: ContainerRuntime, id: &str) -> Option<String> {
    match runtime {
        ContainerRuntime::Docker => read_docker_name(id),
        ContainerRuntime::Podman => read_podman_name(id),
        // Containerd / CRI keep state in a bbolt database we can't
        // sensibly parse without the runtime's libraries. Stick with
        // the short id for now; k8s pod name resolution would need
        // kubelet API access which is outside v1 scope.
        ContainerRuntime::Containerd => None,
        // LXC's "id" *is* the name — no lookup needed.
        ContainerRuntime::Lxc => Some(id.to_string()),
        ContainerRuntime::Other => None,
    }
}

/// Read `/var/lib/docker/containers/<id>/config.v2.json` and pull out
/// the `Name` field. Docker stores names with a leading slash
/// (e.g. `/web-app`) which we strip for display.
fn read_docker_name(id: &str) -> Option<String> {
    let path = format!("/var/lib/docker/containers/{}/config.v2.json", id);
    let bytes = std::fs::read(Path::new(&path)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = v.get("Name")?.as_str()?;
    Some(raw.trim_start_matches('/').to_string())
}

/// Read `/var/lib/containers/storage/overlay-containers/containers.json`
/// once and find the entry with this id. Podman keeps all containers
/// in one JSON array, so we scan it. Cheap enough for the rare
/// resolution path; we cache the result anyway.
fn read_podman_name(id: &str) -> Option<String> {
    let path = "/var/lib/containers/storage/overlay-containers/containers.json";
    let bytes = std::fs::read(Path::new(path)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let arr = v.as_array()?;
    for entry in arr {
        if entry.get("id").and_then(|s| s.as_str()) == Some(id) {
            // Podman stores names as a JSON array; first element is
            // the canonical name.
            if let Some(names) = entry.get("names").and_then(|n| n.as_array()) {
                if let Some(first) = names.first().and_then(|n| n.as_str()) {
                    return Some(first.to_string());
                }
            }
        }
    }
    None
}

/// Parse cgroup info (full path + leaf) into a `Container`, performing
/// name resolution against the supplied resolver. Returns `None` when
/// the cgroup doesn't look like a container — host processes,
/// systemd units, user sessions all fall through.
pub fn detect(
    cgroup_leaf: Option<&str>,
    cgroup_full: Option<&str>,
    resolver: &mut NameResolver,
) -> Option<Container> {
    let (runtime, id) = if let Some(leaf) = cgroup_leaf {
        parse_cgroup_leaf(leaf).or_else(|| cgroup_full.and_then(parse_cgroup_path))
    } else {
        cgroup_full.and_then(parse_cgroup_path)
    }?;
    let mut c = Container { runtime, id, name: None };
    resolver.resolve(&mut c);
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_systemd_scope() {
        let r = parse_cgroup_leaf("docker-abc123def456.scope");
        assert_eq!(
            r,
            Some((ContainerRuntime::Docker, "abc123def456".to_string()))
        );
    }

    #[test]
    fn parses_podman_libpod_scope() {
        let r = parse_cgroup_leaf("libpod-deadbeef.scope");
        assert_eq!(r, Some((ContainerRuntime::Podman, "deadbeef".to_string())));
    }

    #[test]
    fn parses_cri_containerd() {
        let r = parse_cgroup_leaf("cri-containerd-feedface.scope");
        assert_eq!(
            r,
            Some((ContainerRuntime::Containerd, "feedface".to_string()))
        );
    }

    #[test]
    fn parses_bare_containerd() {
        let r = parse_cgroup_leaf("containerd-cafef00d.scope");
        assert_eq!(
            r,
            Some((ContainerRuntime::Containerd, "cafef00d".to_string()))
        );
    }

    #[test]
    fn rejects_non_container_systemd_units() {
        // Real systemd leaves seen in the wild — must not match.
        for leaf in &[
            "firefox.service",
            "user@1000.service",
            "session-3.scope",
            "init.scope",
            "system.slice",
        ] {
            assert!(parse_cgroup_leaf(leaf).is_none(), "unexpected match: {leaf}");
        }
    }

    #[test]
    fn parses_docker_cgroupfs_path() {
        let r = parse_cgroup_path("/docker/abc123def456789");
        assert_eq!(
            r,
            Some((ContainerRuntime::Docker, "abc123def456789".to_string()))
        );
    }

    #[test]
    fn parses_lxc_path() {
        let r = parse_cgroup_path("/lxc/myhost");
        assert_eq!(r, Some((ContainerRuntime::Lxc, "myhost".to_string())));
        let r = parse_cgroup_path("/lxc.payload.alpine");
        assert_eq!(r, Some((ContainerRuntime::Lxc, "alpine".to_string())));
    }

    #[test]
    fn name_resolver_falls_back_to_short_id_for_unknown_runtime() {
        // No filesystem read for unknown id; resolver leaves name None,
        // and Container::display() formats as `runtime:short_id`.
        let mut r = NameResolver::new();
        let mut c = Container {
            runtime: ContainerRuntime::Containerd,
            id: "abcdef0123456789".to_string(),
            name: None,
        };
        r.resolve(&mut c);
        assert!(c.name.is_none());
        assert_eq!(c.display(), "containerd:abcdef012345");
    }

    #[test]
    fn name_resolver_uses_lxc_id_as_name() {
        let mut r = NameResolver::new();
        let mut c = Container {
            runtime: ContainerRuntime::Lxc,
            id: "alpine-test".to_string(),
            name: None,
        };
        r.resolve(&mut c);
        assert_eq!(c.name.as_deref(), Some("alpine-test"));
    }

    #[test]
    fn name_resolver_caches_repeated_lookups() {
        // Hit a guaranteed-missing Docker id twice; the second resolve
        // should be a cache hit (we can't easily observe that from
        // here, but at minimum the call must not panic and must
        // produce the same output).
        let mut r = NameResolver::new();
        let mk = || Container {
            runtime: ContainerRuntime::Docker,
            id: "00000000000000000000".to_string(),
            name: None,
        };
        let mut a = mk();
        let mut b = mk();
        r.resolve(&mut a);
        r.resolve(&mut b);
        assert_eq!(a.name, b.name);
    }
}
