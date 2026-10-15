use super::{automatic_target, normalize_target, Selection, Snapshot, Transport, FRESHNESS};
use std::collections::BTreeMap;

pub struct Registry {
    daemon_epoch: String,
    topology_epoch: u64,
    source_epoch: u64,
    clock: u64,
    topology_seen: Option<u64>,
    sources: BTreeMap<String, Transport>,
    onus: BTreeMap<u16, String>,
    links: BTreeMap<String, String>,
}

impl Registry {
    pub fn new(daemon_epoch: String) -> Self {
        Self {
            daemon_epoch,
            topology_epoch: 0,
            source_epoch: 0,
            clock: 0,
            topology_seen: None,
            sources: BTreeMap::new(),
            onus: BTreeMap::new(),
            links: BTreeMap::new(),
        }
    }

    fn advance(&mut self, time: u64) {
        if time < self.clock {
            self.sources.clear();
            self.links.clear();
            self.topology_seen = None;
            self.topology_epoch = self.topology_epoch.saturating_add(1);
        }
        self.clock = time;
    }

    pub fn observe(&mut self, topic: &str, time: u64) {
        let Some(mac) = normalize_target(topic) else {
            return;
        };
        self.advance(time);
        if !self.sources.contains_key(&mac) && self.sources.len() >= 64 {
            let oldest = self
                .sources
                .iter()
                .filter(|(source, _)| !self.links.contains_key(*source))
                .min_by_key(|(_, t)| t.last_seen)
                .map(|(source, _)| source.clone());
            match oldest {
                Some(source) => {
                    self.sources.remove(&source);
                }
                None => return,
            }
        }
        let observation = self.sources.entry(mac).or_default();
        if observation.epoch == 0 || time.saturating_sub(observation.last_seen) > FRESHNESS {
            self.source_epoch = self.source_epoch.saturating_add(1);
            observation.epoch = self.source_epoch;
        }
        observation.last_seen = time;
    }

    pub fn update_topology(
        &mut self,
        onus: BTreeMap<u16, String>,
        observation: Result<BTreeMap<String, String>, String>,
        time: u64,
    ) {
        self.advance(time);
        let links = observation.unwrap_or_default();
        if self.onus != onus || self.links != links {
            self.topology_epoch = self.topology_epoch.saturating_add(1);
        }
        self.onus = onus;
        self.links = links;
        self.topology_seen = Some(time);
    }

    pub fn snapshots(&self, time: u64) -> Vec<Snapshot> {
        self.sources
            .iter()
            .map(|(mac, source)| {
                let serial = self.links.get(mac);
                let mut onus = self.onus.iter().filter(|(_, sn)| Some(*sn) == serial);
                let onu = match (onus.next(), onus.next()) {
                    (Some((id, _)), None) => Some(*id),
                    _ => None,
                };
                let fresh = time >= self.clock
                    && source.last_seen <= time
                    && time - source.last_seen <= FRESHNESS
                    && self
                        .topology_seen
                        .is_some_and(|seen| seen <= time && time - seen <= FRESHNESS);
                let reason = match (fresh, serial, onu) {
                    (false, _, _) => "stale",
                    (true, Some(sn), Some(_)) => {
                        match automatic_target(&self.sources, &self.links, (sn, time)) {
                            Ok(Some(_)) => "ready",
                            Err(_) => "ambiguous",
                            Ok(None) => "unmatched",
                        }
                    }
                    _ => "unmatched",
                };
                Snapshot {
                    mqtt_mac: mac.clone(),
                    optical_sn: serial.cloned(),
                    onu_id: onu,
                    binding_generation: format!(
                        "{}-{}-{}-{mac}",
                        self.daemon_epoch, self.topology_epoch, source.epoch
                    ),
                    last_seen: source.last_seen,
                    reason: reason.into(),
                }
            })
            .collect()
    }

    pub fn resolve(&self, selection: &Selection, time: u64) -> Result<Snapshot, &'static str> {
        self.snapshots(time)
            .into_iter()
            .find(|snapshot| {
                snapshot.reason == "ready"
                    && snapshot.optical_sn.as_deref() == Some(selection.optical_sn.as_str())
                    && snapshot.onu_id == Some(selection.onu_id)
                    && snapshot.binding_generation == selection.binding_generation
            })
            .ok_or("stale or unresolved MQTT target; refresh the device selection")
    }
}
