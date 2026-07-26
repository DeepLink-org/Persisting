//! Read placement from **torchrun** (or compatible) environment.
//!
//! Semantic block: [`crate::blocks::ids::PLACEMENT`] (distributed naming + seed).
//!
//! We do **not** spawn processes ourselves. Launch with:
//!
//! ```bash
//! torchrun --nproc_per_node=4 -- persisting ppilot plan.py
//! ```

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct DistEnv {
    pub rank: usize,
    pub world_size: usize,
    pub master_addr: String,
    /// Where rank0 binds Pulsing so peers can join (no custom rdzv protocol).
    pub pulsing_seed: SocketAddr,
}

impl DistEnv {
    /// `Some` when launched under torchrun (`RANK` + `WORLD_SIZE` present).
    pub fn from_env() -> Result<Option<Self>> {
        let map: HashMap<String, String> = env::vars().collect();
        Self::from_map(&map)
    }

    /// Testable entry: build from an explicit env map (no process-global mutation).
    pub fn from_map(env: &HashMap<String, String>) -> Result<Option<Self>> {
        let rank = match env.get("RANK") {
            Some(v) => v.parse::<usize>().context("parse RANK")?,
            None => return Ok(None),
        };
        let world_size = env
            .get("WORLD_SIZE")
            .context("WORLD_SIZE required when RANK is set (use torchrun)")?
            .parse::<usize>()
            .context("parse WORLD_SIZE")?;
        if world_size == 0 {
            bail!("WORLD_SIZE must be >= 1");
        }
        if rank >= world_size {
            bail!("RANK {rank} >= WORLD_SIZE {world_size}");
        }
        let master_addr = env
            .get("MASTER_ADDR")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".into());
        let master_port: u16 = env
            .get("MASTER_PORT")
            .map(|s| s.as_str())
            .unwrap_or("29500")
            .parse()
            .context("parse MASTER_PORT")?;
        let pulsing_port: u16 = env
            .get("PERSISTING_PULSING_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(master_port.saturating_add(17));
        let pulsing_seed = Self::pulsing_seed_addr(&master_addr, pulsing_port)?;
        Ok(Some(Self {
            rank,
            world_size,
            master_addr,
            pulsing_seed,
        }))
    }

    pub fn pulsing_seed_addr(master_addr: &str, pulsing_port: u16) -> Result<SocketAddr> {
        format!("{master_addr}:{pulsing_port}")
            .parse()
            .with_context(|| format!("parse Pulsing seed {master_addr}:{pulsing_port}"))
    }

    pub fn is_driver(&self) -> bool {
        self.rank == 0
    }

    /// Logical worker actor name when `per_worker == 1`.
    pub fn worker_name(rank: usize) -> String {
        format!("ppilot/worker/{rank}")
    }

    /// One concurrent execute slot under a logical worker/rank.
    ///
    /// When `per_worker == 1`, uses the legacy name [`Self::worker_name`] (no `/slot/`).
    pub fn slot_name(worker: usize, slot: usize, per_worker: usize) -> String {
        if per_worker <= 1 {
            Self::worker_name(worker)
        } else {
            format!("ppilot/worker/{worker}/slot/{slot}")
        }
    }

    /// Flat pool index in **slot-major** order (matches [`Self::slot_names`]).
    ///
    /// `index = slot * n_workers + worker` — used by Scheduler / DeathWatch.
    pub fn slot_flat_index(
        worker: usize,
        slot: usize,
        n_workers: usize,
        per_worker: usize,
    ) -> usize {
        let per_worker = per_worker.max(1);
        let n_workers = n_workers.max(1);
        debug_assert!(
            worker < n_workers,
            "worker {worker} >= n_workers {n_workers}"
        );
        debug_assert!(slot < per_worker, "slot {slot} >= per_worker {per_worker}");
        slot.saturating_mul(n_workers).saturating_add(worker)
    }

    /// Flat pool names in **slot-major** order: all workers' slot0, then slot1, …
    pub fn slot_names(n_workers: usize, per_worker: usize) -> Vec<String> {
        let per_worker = per_worker.max(1);
        let n_workers = n_workers.max(1);
        let mut names = Vec::with_capacity(n_workers.saturating_mul(per_worker));
        for slot in 0..per_worker {
            for worker in 0..n_workers {
                names.push(Self::slot_name(worker, slot, per_worker));
            }
        }
        names
    }

    pub fn worker_names(world_size: usize) -> Vec<String> {
        Self::slot_names(world_size, 1)
    }

    /// Side-channel cancel actor for rank (separate mailbox from WorkerActor).
    pub fn job_control_name(rank: usize) -> String {
        format!("ppilot/job_control/{rank}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn from_map_none_without_rank() {
        assert!(DistEnv::from_map(&HashMap::new()).unwrap().is_none());
    }

    #[test]
    fn from_map_parses_torchrun_defaults() {
        let env = map(&[
            ("RANK", "1"),
            ("WORLD_SIZE", "4"),
            ("MASTER_ADDR", "10.0.0.1"),
        ]);
        let d = DistEnv::from_map(&env).unwrap().unwrap();
        assert_eq!(d.rank, 1);
        assert_eq!(d.world_size, 4);
        assert!(!d.is_driver());
        assert_eq!(d.pulsing_seed.port(), 29500 + 17);
        assert_eq!(d.master_addr, "10.0.0.1");
    }

    #[test]
    fn from_map_rejects_bad_rank() {
        let env = map(&[("RANK", "4"), ("WORLD_SIZE", "4")]);
        assert!(DistEnv::from_map(&env).is_err());
    }

    #[test]
    fn pulsing_port_override() {
        let env = map(&[
            ("RANK", "0"),
            ("WORLD_SIZE", "2"),
            ("MASTER_PORT", "30000"),
            ("PERSISTING_PULSING_PORT", "31000"),
        ]);
        let d = DistEnv::from_map(&env).unwrap().unwrap();
        assert!(d.is_driver());
        assert_eq!(d.pulsing_seed.port(), 31000);
    }

    #[test]
    fn slot_names_slot_major() {
        assert_eq!(
            DistEnv::slot_names(2, 2),
            vec![
                "ppilot/worker/0/slot/0",
                "ppilot/worker/1/slot/0",
                "ppilot/worker/0/slot/1",
                "ppilot/worker/1/slot/1",
            ]
        );
        assert_eq!(
            DistEnv::slot_names(2, 1),
            vec!["ppilot/worker/0", "ppilot/worker/1"]
        );
    }

    #[test]
    fn slot_flat_index_matches_slot_names_order() {
        let names = DistEnv::slot_names(3, 2);
        for worker in 0..3 {
            for slot in 0..2 {
                let i = DistEnv::slot_flat_index(worker, slot, 3, 2);
                assert_eq!(names[i], DistEnv::slot_name(worker, slot, 2));
            }
        }
        // Regression: rank0's second slot is NOT flat index 1 when world>1.
        assert_eq!(DistEnv::slot_flat_index(0, 1, 2, 2), 2);
        assert_eq!(DistEnv::slot_flat_index(1, 0, 2, 2), 1);
    }
}
