//! Initial placement policy, separate from SC configuration and lifecycle.
//! Counts reservations, not measured utilization; there is no runtime balancing.
pub(crate) const MAX_CPUS: usize = 64;

pub(crate) fn initial_cpu(
    cpus: usize,
    host_cpu: usize,
    reserved: impl Iterator<Item = usize>,
) -> usize {
    assert!((1..=MAX_CPUS).contains(&cpus) && host_cpu < cpus);
    let mut load = [0usize; MAX_CPUS];
    // The root-hosted manager is not itself a managed process.
    load[host_cpu] = 1;
    for cpu in reserved {
        assert!(cpu < cpus);
        load[cpu] += 1;
    }
    (0..cpus).min_by_key(|&cpu| (load[cpu], cpu)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_cpu_overcommits_without_manual_configuration() {
        assert_eq!(initial_cpu(1, 0, [0, 0, 0].into_iter()), 0);
    }
    #[test]
    fn fills_idle_cpus_then_balances_reservations() {
        assert_eq!(initial_cpu(2, 0, [].into_iter()), 1);
        assert_eq!(initial_cpu(2, 0, [1].into_iter()), 0);
        assert_eq!(initial_cpu(2, 0, [1, 0].into_iter()), 1);
        assert_eq!(initial_cpu(4, 0, [1, 2].into_iter()), 3);
    }
    #[test]
    fn host_reservation_uses_actual_boot_cpu() {
        assert_eq!(initial_cpu(2, 1, [].into_iter()), 0);
    }
}
