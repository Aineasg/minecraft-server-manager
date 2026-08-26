//! Turning a single "everything must fit in N GiB" number into concrete JVM and
//! cgroup limits.
//!
//! The user gives one hard ceiling for **app + JVM + world combined**. From it we
//! derive:
//!
//! * a `MemoryMax` for the systemd scope the server runs in — the kernel's
//!   last-resort SIGKILL line, which should never actually be hit;
//! * a lower `MemoryHigh` — where the kernel starts applying reclaim pressure,
//!   giving the JVM a chance to GC instead of dying;
//! * `-Xmx` / `-Xms` for the JVM itself.
//!
//! The key fact the arithmetic encodes: a server JVM's real resident size is
//! roughly `Xmx * 1.25 + ~512 MiB` once you count metaspace, the code cache, G1
//! bookkeeping, ~50-100 worldgen/Netty thread stacks and Netty's direct
//! buffers. Setting `-Xmx` anywhere near the ceiling therefore OOM-kills the
//! process. We size `-Xmx` so the *projected* resident size stays under
//! `MemoryHigh`, and clamp the user-adjustable maximum so it stays under
//! `MemoryMax`.

/// Default overall ceiling: 9 GiB, as specified for this deployment.
pub const DEFAULT_TOTAL_MIB: u64 = 9 * 1024;

/// Held back for the GTK app process and kernel slack, outside the server scope.
const APP_RESERVE_MIB: u64 = 1024;

/// Fixed non-heap JVM cost (metaspace, code cache, thread stacks, direct buffers).
const JVM_OVERHEAD_MIB: u64 = 512;

/// Extra safety margin kept below `MemoryMax` when computing the `-Xmx` ceiling.
const HARD_CAP_MARGIN_MIB: u64 = 512;

/// Smallest heap we will ever propose.
const XMX_FLOOR_MIB: u64 = 1024;

/// Project a JVM's resident set size for a given heap, in MiB.
#[must_use]
pub fn projected_jvm_rss_mib(xmx_mib: u64) -> u64 {
    xmx_mib * 5 / 4 + JVM_OVERHEAD_MIB
}

/// A fully resolved set of limits ready to hand to systemd and the JVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    /// The overall ceiling the user set (app + JVM + world).
    pub total_mib: u64,
    /// `MemoryMax` for the server's systemd scope.
    pub scope_max_mib: u64,
    /// `MemoryHigh` for the server's systemd scope.
    pub scope_high_mib: u64,
    /// Chosen `-Xmx`.
    pub xmx_mib: u64,
    /// Chosen `-Xms` (kept below `-Xmx`: with no `AlwaysPreTouch` there is no
    /// benefit to committing the whole heap up front, and a smaller initial
    /// commit is safer under the cap).
    pub xms_mib: u64,
    /// Upper bound the UI slider must enforce for `-Xmx`.
    pub xmx_max_mib: u64,
    /// Lower bound for the `-Xmx` slider.
    pub xmx_min_mib: u64,
    /// False when the ceiling is too low to run a server at all
    /// (`xmx_max_mib < xmx_min_mib`).
    pub feasible: bool,
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self::new(DEFAULT_TOTAL_MIB, None)
    }
}

impl MemoryBudget {
    /// Derive limits from a total ceiling and an optional requested heap size.
    ///
    /// `requested_xmx_mib` is clamped into `[xmx_min_mib, xmx_max_mib]`. When
    /// `None`, a default heap is chosen that keeps the projected RSS under
    /// `MemoryHigh`.
    #[must_use]
    pub fn new(total_mib: u64, requested_xmx_mib: Option<u64>) -> Self {
        let scope_max_mib = total_mib.saturating_sub(APP_RESERVE_MIB);
        let scope_high_mib = scope_max_mib * 7 / 8;

        // Largest heap whose projected RSS stays a margin below the hard cap.
        let xmx_max_mib = floor_to(
            invert_rss(scope_max_mib.saturating_sub(HARD_CAP_MARGIN_MIB)),
            128,
        );
        let xmx_min_mib = XMX_FLOOR_MIB;
        let feasible = xmx_max_mib >= xmx_min_mib;

        let default_xmx = floor_to(invert_rss(scope_high_mib), 512)
            .clamp(xmx_min_mib.min(xmx_max_mib), xmx_max_mib);

        let xmx_mib = match requested_xmx_mib {
            Some(req) => req.clamp(xmx_min_mib.min(xmx_max_mib), xmx_max_mib),
            None => default_xmx,
        };
        let xms_mib = xmx_mib.min(1024);

        Self {
            total_mib,
            scope_max_mib,
            scope_high_mib,
            xmx_mib,
            xms_mib,
            xmx_max_mib,
            xmx_min_mib,
            feasible,
        }
    }

    /// Projected resident size of the JVM with the chosen heap.
    #[must_use]
    pub fn projected_jvm_rss_mib(&self) -> u64 {
        projected_jvm_rss_mib(self.xmx_mib)
    }

    /// `-p KEY=VALUE` arguments for `systemd-run` that pin the scope's memory.
    #[must_use]
    pub fn systemd_properties(&self) -> Vec<String> {
        vec![
            format!("MemoryMax={}M", self.scope_max_mib),
            format!("MemoryHigh={}M", self.scope_high_mib),
            "MemorySwapMax=0".to_string(),
        ]
    }
}

/// Given a target RSS, return the heap size that projects to it (inverse of
/// [`projected_jvm_rss_mib`]).
fn invert_rss(target_rss_mib: u64) -> u64 {
    target_rss_mib.saturating_sub(JVM_OVERHEAD_MIB) * 4 / 5
}

fn floor_to(value: u64, step: u64) -> u64 {
    value - value % step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_matches_the_9gib_design() {
        let b = MemoryBudget::default();
        assert_eq!(b.scope_max_mib, 8192);
        assert_eq!(b.scope_high_mib, 7168);
        assert_eq!(b.xmx_mib, 5120);
        assert_eq!(b.xms_mib, 1024);
        assert_eq!(b.xmx_max_mib, 5632);
        assert!(b.feasible);
    }

    #[test]
    fn projected_rss_of_the_max_heap_stays_under_the_hard_cap() {
        let b = MemoryBudget::default();
        assert!(projected_jvm_rss_mib(b.xmx_max_mib) <= b.scope_max_mib);
    }

    #[test]
    fn projected_rss_of_the_default_heap_stays_under_memory_high() {
        let b = MemoryBudget::default();
        assert!(b.projected_jvm_rss_mib() <= b.scope_high_mib);
    }

    #[test]
    fn requested_heap_is_clamped_both_ways() {
        assert_eq!(
            MemoryBudget::new(DEFAULT_TOTAL_MIB, Some(64_000)).xmx_mib,
            5632
        );
        assert_eq!(
            MemoryBudget::new(DEFAULT_TOTAL_MIB, Some(256)).xmx_mib,
            1024
        );
        assert_eq!(
            MemoryBudget::new(DEFAULT_TOTAL_MIB, Some(4096)).xmx_mib,
            4096
        );
    }

    #[test]
    fn systemd_properties_are_well_formed() {
        let props = MemoryBudget::default().systemd_properties();
        assert_eq!(props[0], "MemoryMax=8192M");
        assert_eq!(props[1], "MemoryHigh=7168M");
        assert_eq!(props[2], "MemorySwapMax=0");
    }

    #[test]
    fn a_tiny_ceiling_is_reported_infeasible() {
        let b = MemoryBudget::new(2048, None);
        assert!(!b.feasible);
    }

    #[test]
    fn a_generous_ceiling_scales_up() {
        let b = MemoryBudget::new(16 * 1024, None);
        assert!(b.xmx_max_mib > 10 * 1024);
        assert!(b.feasible);
    }
}
